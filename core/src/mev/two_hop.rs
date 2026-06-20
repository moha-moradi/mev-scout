//! Two-hop arbitrage detection — finds cyclic arbitrage across two connected pools (V2↔V2, V2↔V3, V3↔V3).

use std::cmp;

use alloy::primitives::{Address, U256};

use crate::mev::opportunity::MevOpportunity;
use crate::pool::math::{constant_product_output_amount, optimal_two_hop_arb, optimal_two_hop_arb_generic, TwoHopArbResult};
use crate::pool::state::{calldata_gas_estimate, BalancerPoolState, CurvePoolState, PoolManager, PoolState, UniswapV2PoolState};
use crate::pool::v3_quote::{estimate_v3_swap_gas, quote_v3_exact_in, max_v3_tradeable_amount};
use crate::types::{GasConfig, Strategy};

/// Detects two-hop arbitrage opportunities across V2, V3, and mixed pools.
///
/// Uses analytical closed-form solutions for V2 pairs and a step-by-step quote
/// engine for V3 pools. Maintains a per-block dedup set so the same persistent
/// arb gap is not re-reported across multiple transactions in the same block.
pub struct TwoHopArbDetector {
    block_number: u64,
    seen: std::collections::HashSet<(Address, Address, Address, Address)>,
}

impl TwoHopArbDetector {
    /// Create a new detector for the given block.
    /// The `seen` set is fresh each block, so opportunities can be re-detected
    /// on the next block if the price gap persists.
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            seen: std::collections::HashSet::new(),
        }
    }

    /// Check all arbitrage pool-pair directions and emit profitable two-hop opportunities.
    /// Deduplicates per block: each unique (pool_a, pool_b, token_in, token_out) is emitted
    /// at most once, preventing the same persistent arb gap from being re-reported across
    /// multiple transactions.
    pub fn detect(
        &mut self,
        pool_manager: &PoolManager,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();
        let pairs = pool_manager.arbitrage_pairs();

        for (pool_a, pool_b, shared_token) in &pairs {
            if let Some(opp) = Self::check_direction(
                pool_manager, *pool_a, *pool_b, *shared_token,
                self.block_number, tx_index, timestamp,
                base_fee_per_gas, gas_config,
            ) {
                let key = (opp.pool_a, opp.pool_b, opp.token_in, opp.token_out);
                if self.seen.insert(key) {
                    opportunities.push(opp);
                }
            }
            if let Some(opp) = Self::check_direction(
                pool_manager, *pool_b, *pool_a, *shared_token,
                self.block_number, tx_index, timestamp,
                base_fee_per_gas, gas_config,
            ) {
                let key = (opp.pool_a, opp.pool_b, opp.token_in, opp.token_out);
                if self.seen.insert(key) {
                    opportunities.push(opp);
                }
            }
        }

        opportunities
    }

    #[allow(clippy::too_many_arguments)]
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

        let (token_in, token_out) = arb_tokens(pool_a, pool_b, shared_token)?;

        let result = quote_path(pool_a, pool_b, shared_token)?;

        if result.profit == 0 {
            return None;
        }

        let gas_limit = estimate_gas_for_two_hop(pool_a, pool_b, shared_token);
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
            let native_profit = pm.normalize_to_native(token_out, profit_after_fl)
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
            pga_adjusted_profit: None,
            gas_cost_wei,
            timestamp,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
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
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::UniswapV2(a), PoolState::UniswapV3(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let zero_b = shared_token == b.info.token0;
            let max_input = r_a_other;
            let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::UniswapV2(b)) => {
            let zero_a = shared_token == a.info.token1;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            let max_input = r_b_out;
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Curve(a), PoolState::Curve(b)) => {
            let max_input = a.balances[*a.token_index.get(&token_in)?];
            let quote_a = |x: u128| curve_output_amount(x, a, token_in, shared_token);
            let quote_b = |x: u128| curve_output_amount(x, b, shared_token, token_out);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::Balancer(b)) => {
            let (b_a_in, b_a_out, fee_a) = balancer_reserves(a, token_in, shared_token)?;
            let (b_b_in, b_b_out, fee_b) = balancer_reserves(b, shared_token, token_out)?;
            let (wa_in, wa_out) = balancer_weights(a, token_in, shared_token);
            let (wb_in, wb_out) = balancer_weights(b, shared_token, token_out);
            let max_input = b_a_in;
            let quote_a = |x: u128| balancer_output_amount(x, b_a_in, b_a_out, wa_in, wa_out, fee_a);
            let quote_b = |x: u128| balancer_output_amount(x, b_b_in, b_b_out, wb_in, wb_out, fee_b);
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
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::Curve(b)) => {
            let zero_a = shared_token == a.info.token1;
            let max_input = max_v3_tradeable_amount(a, zero_a);
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| curve_output_amount(x, b, shared_token, token_out);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::UniswapV2(b)) => {
            let (b_a_in, b_a_out, fee_a) = balancer_reserves(a, token_in, shared_token)?;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            let (wa_in, wa_out) = balancer_weights(a, token_in, shared_token);
            let quote_a = |x: u128| balancer_output_amount(x, b_a_in, b_a_out, wa_in, wa_out, fee_a);
            let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
            optimal_two_hop_arb_generic(b_a_in, &quote_a, &quote_b)
        }
        (PoolState::UniswapV2(a), PoolState::Balancer(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let (b_b_in, b_b_out, fee_b) = balancer_reserves(b, shared_token, token_out)?;
            let (wb_in, wb_out) = balancer_weights(b, shared_token, token_out);
            let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
            let quote_b = |x: u128| balancer_output_amount(x, b_b_in, b_b_out, wb_in, wb_out, fee_b);
            optimal_two_hop_arb_generic(r_a_other, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::UniswapV3(b)) => {
            let (b_a_in, b_a_out, fee_a) = balancer_reserves(a, token_in, shared_token)?;
            let zero_b = shared_token == b.info.token0;
            let (wa_in, wa_out) = balancer_weights(a, token_in, shared_token);
            let quote_a = |x: u128| balancer_output_amount(x, b_a_in, b_a_out, wa_in, wa_out, fee_a);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            optimal_two_hop_arb_generic(b_a_in, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::Balancer(b)) => {
            let zero_a = shared_token == a.info.token1;
            let (b_b_in, b_b_out, fee_b) = balancer_reserves(b, shared_token, token_out)?;
            let (wb_in, wb_out) = balancer_weights(b, shared_token, token_out);
            let max_input = max_v3_tradeable_amount(a, zero_a);
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| balancer_output_amount(x, b_b_in, b_b_out, wb_in, wb_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
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
    let eval = |input: u128| {
        match two_hop_profit_at(pool_a, pool_b, shared_token, input) {
            Some(p) if p > 0 => Some(U256::from(p)),
            _ => None,
        }
    };
    (
        eval(optimal_input.saturating_mul(101) / 100),   // +1%
        eval(optimal_input.saturating_mul(99) / 100),    // -1%
        eval(optimal_input.saturating_mul(102) / 100),   // +2%
        eval(optimal_input.saturating_mul(98) / 100),    // -2%
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
        PoolState::Curve(a) => {
            curve_output_amount(input_amount, a, token_in, shared_token)?
        }
        PoolState::Balancer(a) => {
            let (reserve_in, reserve_out, fee) = balancer_reserves(a, token_in, shared_token)?;
            let (w_in, w_out) = balancer_weights(a, token_in, shared_token);
            balancer_output_amount(input_amount, reserve_in, reserve_out, w_in, w_out, fee)?
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
        PoolState::Curve(b) => {
            curve_output_amount(intermediate, b, shared_token, token_out)?
        }
        PoolState::Balancer(b) => {
            let (reserve_in, reserve_out, fee) = balancer_reserves(b, shared_token, token_out)?;
            let (w_in, w_out) = balancer_weights(b, shared_token, token_out);
            balancer_output_amount(intermediate, reserve_in, reserve_out, w_in, w_out, fee)?
        }
    };

    if output > input_amount { Some(output - input_amount) } else { None }
}

/// Extract the token_in (spent) and token_out (received) for a two-hop arb
/// given two pools that share a common token.
///
/// For multi-token pools (Curve 3pool, Balancer weighted pools with 3+ tokens),
/// falls back to the pool's `token_index` map when the shared token is not
/// in the first two positions, picking the first eligible non-shared token.
fn arb_tokens(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
) -> Option<(Address, Address)> {
    let info_a = pool_a.info();
    let info_b = pool_b.info();

    let token_in = if info_a.token0 == shared_token {
        info_a.token1
    } else if info_a.token1 == shared_token {
        info_a.token0
    } else {
        // Multi-token pool: pick the smallest non-shared token address
        // for deterministic selection (avoids HashMap iteration order).
        let non_shared = match pool_a {
            PoolState::Curve(c) => c.token_index.keys()
                .filter(|k| **k != shared_token && !k.is_zero())
                .min()
                .copied(),
            PoolState::Balancer(b) => b.token_index.keys()
                .filter(|k| **k != shared_token && !k.is_zero())
                .min()
                .copied(),
            _ => None,
        };
        non_shared?
    };

    let token_out = if info_b.token0 == shared_token {
        info_b.token1
    } else if info_b.token1 == shared_token {
        info_b.token0
    } else {
        let non_shared = match pool_b {
            PoolState::Curve(c) => c.token_index.keys()
                .filter(|k| **k != shared_token && !k.is_zero())
                .min()
                .copied(),
            PoolState::Balancer(b) => b.token_index.keys()
                .filter(|k| **k != shared_token && !k.is_zero())
                .min()
                .copied(),
            _ => None,
        };
        non_shared?
    };

    Some((token_in, token_out))
}

/// StableSwap output amount using Newton's method for the invariant D (C3).
///
/// Handles Curve pools with any number of tokens (n ≥ 2) by computing the
/// generalized StableSwap invariant:
///   A · nⁿ · Σxᵢ + D = A · nⁿ · D + Dⁿ⁺¹ / (nⁿ · Πxᵢ)
///
/// Uses f64 arithmetic — the result is a profit estimate, which does not
/// require exact EVM precision. The Newton iteration converges quickly
/// (typically < 32 steps) to machine epsilon.
pub fn curve_output_amount(
    amount_in: u128,
    pool: &CurvePoolState,
    token_in: Address,
    token_out: Address,
) -> Option<u128> {
    let n = pool.balances.len();
    if n < 2 || amount_in == 0 {
        return None;
    }
    let idx_in = *pool.token_index.get(&token_in)?;
    let idx_out = *pool.token_index.get(&token_out)?;
    let balances: Vec<f64> = pool.balances.iter().map(|&b| b as f64).collect();
    if balances[idx_in] <= 0.0 || balances[idx_out] <= 0.0 {
        return None;
    }

    let a = pool.a_coeff as f64;
    let nn = (n as f64).powf(n as f64); // nⁿ
    let fee_factor = 1.0 - (pool.info.fee as f64) / 1_000_000.0;

    // --- Phase 1: Compute invariant D from all balances (Newton's method) ---
    // f(D) = D^(n+1) / (nⁿ * prod) + (A·nⁿ - 1)·D - A·nⁿ·sum = 0
    // f'(D) = (n+1)·D^n / (nⁿ * prod) + (A·nⁿ - 1)
    let sum: f64 = balances.iter().sum();
    let prod: f64 = balances.iter().product();
    if prod <= 0.0 {
        return None;
    }
    let ann = a * nn;
    let d_init = sum;
    let d = newton_stableswap_invariant(n, ann, sum, prod, d_init)?;

    // --- Phase 2: Apply fee to input ---
    let x_in_new = balances[idx_in] + amount_in as f64 * fee_factor;

    // --- Phase 3: Solve for x_out' (Newton) ---
    // We need to find x_out' such that D is unchanged given x_in_new and all
    // other balances.  The equation in x (the unknown output token balance) is:
    //   A·nⁿ·(x + S) + D = A·nⁿ·D + Dⁿ⁺¹ / (nⁿ · x · P)
    // where S = sum of other balances (including x_in_new), P = product of others.
    let sum_others: f64 = balances.iter().enumerate()
        .filter(|&(i, _)| i != idx_out)
        .map(|(_, &v)| v)
        .sum::<f64>() + (x_in_new - balances[idx_in]); // replace idx_in with x_in_new
    let prod_others: f64 = balances.iter().enumerate()
        .filter(|&(i, _)| i != idx_out && i != idx_in)
        .map(|(_, &v)| v)
        .product::<f64>() * x_in_new;
    if prod_others <= 0.0 {
        return None;
    }

    let x_out_new = newton_stableswap_output(n, ann, d, sum_others, prod_others)?;

    let output = balances[idx_out] - x_out_new;
    if output <= 0.0 { None } else { Some(output as u128) }
}

/// Newton's method to find the StableSwap invariant D from N balances.
///
/// Solves:  f(D) = D^(n+1) / (nⁿ·P)  + (A·nⁿ - 1)·D - A·nⁿ·S = 0
/// where S = sum(balances), P = prod(balances), n = number of tokens.
fn newton_stableswap_invariant(
    n: usize,
    ann: f64,
    sum: f64,
    prod: f64,
    guess: f64,
) -> Option<f64> {
    let nf = n as f64;
    let np1 = (n + 1) as f64;
    let denom = prod * nf.powf(nf); // nⁿ · P
    if denom <= 0.0 {
        return None;
    }
    let c = ann - 1.0;
    let target = ann * sum;
    let mut d = guess;
    for _ in 0..128 {
        let d_np1 = d.powf(np1);    // D^(n+1)
        let d_n = d.powf(nf);       // D^n
        let f = d_np1 / denom + c * d - target;
        let deriv = np1 * d_n / denom + c;
        if deriv.abs() < 1e-30 { break; }
        let d_next = d - f / deriv;
        if (d_next - d).abs() <= 1.0 { d = d_next; break; }
        if d_next <= 0.0 { break; }
        d = d_next;
    }
    if d <= 0.0 { None } else { Some(d) }
}

/// Newton's method to find the new output token balance after a swap.
///
/// Solves:  A·nⁿ·(x + S) + D = A·nⁿ·D + Dⁿ⁺¹ / (nⁿ · x · P)
/// Rearranged: f(x) = A·nⁿ·x + A·nⁿ·(S - D) + D - Dⁿ⁺¹ / (nⁿ · P · x) = 0
/// Or:  f(x) = ann·x + ann·(S - D) + D - K/x = 0  where K = Dⁿ⁺¹ / (nⁿ · P)
/// f'(x) = ann + K/x²
fn newton_stableswap_output(
    n: usize,
    ann: f64,
    d: f64,
    sum_others: f64,
    prod_others: f64,
) -> Option<f64> {
    let nf = n as f64;
    let np1 = (n + 1) as f64;
    let denom = prod_others * nf.powf(nf); // nⁿ · P
    if denom <= 0.0 {
        return None;
    }
    let k = d.powf(np1) / denom;       // Dⁿ⁺¹ / (nⁿ · P)
    let b = ann * (sum_others - d) + d; // constant term coefficient

    // Initial guess: use quadratic approximation for the first step
    // ann·x² + b·x - K = 0  →  x = (-b + sqrt(b² + 4·ann·K)) / (2·ann)
    let disc = b * b + 4.0 * ann * k;
    if disc < 0.0 {
        return None;
    }
    let mut x = (-b + disc.sqrt()) / (2.0 * ann);
    if x <= 0.0 {
        return None;
    }

    // Newton refinement: f(x) = ann·x + b - K/x
    // f'(x) = ann + K/x²
    for _ in 0..64 {
        let k_over_x = k / x;
        let f = ann * x + b - k_over_x;
        let deriv = ann + k_over_x / x; // ann + K/x²
        if deriv.abs() < 1e-30 { break; }
        let x_next = x - f / deriv;
        if (x_next - x).abs() <= 0.5 { x = x_next; break; }
        if x_next <= 0.0 { break; }
        x = x_next;
    }

    if x <= 0.0 { None } else { Some(x) }
}

/// Balancer weighted pool output using the weighted product formula.
/// `weights` are in the same order as reserves, in basis points (1e18 each).
/// If `weights` is empty or wrong length, equal weights are assumed.
pub fn balancer_output_amount(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    weight_in: u128,
    weight_out: u128,
    fee: u32,
) -> Option<u128> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return None;
    }
    // Fee: Balancer charges fee on input
    let fee_factor = 1_000_000u128 - fee as u128;
    let amount_after_fee = amount_in.checked_mul(fee_factor)? / 1_000_000;

    // Weighted product invariant: (reserve_in / weight_in)^weight_in * (reserve_out / weight_out)^weight_out = const
    // After swap: (reserve_in + amount_in)^weight_in * (reserve_out - amount_out)^weight_out = const
    //
    // amount_out = reserve_out * (1 - (reserve_in / (reserve_in + amount_in))^(weight_in/weight_out))
    //
    // Using U256 for intermediate computation.
    let r_in = U256::from(reserve_in);
    let r_out = U256::from(reserve_out);
    let w_in = U256::from(if weight_in == 0 { 1e18 as u128 } else { weight_in });
    let w_out = U256::from(if weight_out == 0 { 1e18 as u128 } else { weight_out });
    let amount = U256::from(amount_after_fee);

    let numerator = r_in;
    let denominator = r_in + amount;

    if denominator.is_zero() { return None; }

    // Compute ratio = reserve_in / (reserve_in + amount_in) in fixed-point
    // Then raise to power (weight_in / weight_out)
    // amount_out = reserve_out * (1 - ratio^(w_in/w_out))

    // For simplicity, use f64 for the exponentiation
    let ratio_f64 = numerator.as_limbs()[0] as f64 / denominator.as_limbs()[0] as f64;
    let exp = w_in.as_limbs()[0] as f64 / w_out.as_limbs()[0] as f64;
    let reduction = ratio_f64.powf(exp);

    let output_f64 = r_out.as_limbs()[0] as f64 * (1.0 - reduction);
    if output_f64 <= 0.0 { return None; }

    Some(output_f64 as u128)
}

/// Extract Curve pool reserves for a specific token pair (token_in → token_out).
/// Extract Balancer weights for a specific token pair (token_in → token_out).
/// Returns (weight_in, weight_out). Falls back to equal weights (1e18 each)
/// if the weights vector is empty or doesn't match the token count.
fn balancer_weights(pool: &BalancerPoolState, token_in: Address, token_out: Address) -> (u128, u128) {
    let default_w = 1_000_000_000_000_000_000u128;
    if pool.weights.len() != pool.balances.len() || pool.weights.is_empty() {
        return (default_w, default_w);
    }
    match (pool.token_index.get(&token_in), pool.token_index.get(&token_out)) {
        (Some(&i), Some(&o)) => (pool.weights[i], pool.weights[o]),
        _ => (default_w, default_w),
    }
}

/// Extract Balancer pool reserves for a specific token pair (token_in → token_out).
/// Correctly handles pools with any number of tokens.
fn balancer_reserves(
    pool: &BalancerPoolState,
    token_in: Address,
    token_out: Address,
) -> Option<(u128, u128, u32)> {
    let idx_in = *pool.token_index.get(&token_in)?;
    let idx_out = *pool.token_index.get(&token_out)?;
    Some((pool.balances[idx_in], pool.balances[idx_out], pool.info.fee))
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

/// Estimate the gas limit for a two-hop arbitrage opportunity based on the
/// actual pool types involved and the swap direction (H7).
///
/// For V3 pools, uses direction-aware tick crossing estimation. For V2/Curve/Balancer,
/// uses per-type empirical benchmarks. Includes base overhead and calldata cost.
fn estimate_gas_for_two_hop(pool_a: &PoolState, pool_b: &PoolState, shared_token: Address) -> u64 {
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

    base_overhead + calldata + a_gas + b_gas
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Address, U256};
    use crate::pool::state::{PoolInfo, UniswapV3PoolState};
    use crate::pool::dex_type::DexType;
    use std::collections::HashMap;

    fn wmatic() -> Address { address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270") }
    fn usdc() -> Address { address!("2791bca1f2de4661ed88a30c99a7a9449aa84174") }
    fn usdt() -> Address { address!("c2132d05d31c914a87c6611c10748aeb04b58e8f") }

    fn v2_pool(addr: Address, t0: Address, t1: Address, r0: u128, r1: u128) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: addr, token0: t0, token1: t1, fee: 30,
                name: None, dex_type: DexType::UniswapV2, tick_spacing: None,
                creation_block: 0,
                pool_id: None,
            },
            reserve0: r0, reserve1: r1,
        })
    }

    fn v3_pool(addr: Address, t0: Address, t1: Address, sqrt: U256, tick: i32, liq: u128) -> PoolState {
        PoolState::UniswapV3(UniswapV3PoolState {
            info: PoolInfo {
                address: addr, token0: t0, token1: t1, fee: 30,
                name: None, dex_type: DexType::UniswapV3, tick_spacing: Some(60),
                creation_block: 0,
                pool_id: None,
            },
            sqrt_price_x96: sqrt, tick, liquidity: liq,
            ticks: std::collections::BTreeMap::new(),
            fee_growth_global_0_x128: U256::ZERO,
            fee_growth_global_1_x128: U256::ZERO,
        })
    }

    // ---- arb_tokens ----

    #[test]
    fn test_arb_tokens_shared_token0_of_a_token1_of_b() {
        let a = v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1, 1);
        let b = v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 1, 1);
        let (token_in, token_out) = arb_tokens(&a, &b, wmatic()).unwrap();
        assert_eq!(token_in, usdc());
        assert_eq!(token_out, usdt());
    }

    #[test]
    fn test_arb_tokens_shared_token1_of_a_token0_of_b() {
        let a = v2_pool(address!("1111111111111111111111111111111111111111"), wmatic(), usdc(), 1, 1);
        let b = v2_pool(address!("2222222222222222222222222222222222222222"), usdt(), wmatic(), 1, 1);
        let (token_in, token_out) = arb_tokens(&a, &b, wmatic()).unwrap();
        assert_eq!(token_in, usdc());
        assert_eq!(token_out, usdt());
    }

    #[test]
    fn test_arb_tokens_no_shared_token_returns_none() {
        let a = v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), usdt(), 1, 1);
        let b = v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdc(), 1, 1);
        assert!(arb_tokens(&a, &b, wmatic()).is_none());
    }

    // ---- v2_reserves ----

    #[test]
    fn test_v2_reserves_buy_side_token0_is_shared() {
        let pool = UniswapV2PoolState {
            info: PoolInfo {
                address: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                token0: usdc(), token1: wmatic(), fee: 30,
                name: None, dex_type: DexType::UniswapV2, tick_spacing: None,
                creation_block: 0,
                pool_id: None,
            },
            reserve0: 1_000_000, reserve1: 2_000_000,
        };
        let (other, shared, fee) = v2_reserves(&pool, wmatic(), true).unwrap();
        assert_eq!(other, 1_000_000);
        assert_eq!(shared, 2_000_000);
        assert_eq!(fee, 30);
    }

    #[test]
    fn test_v2_reserves_sell_side_token0_is_shared() {
        let pool = UniswapV2PoolState {
            info: PoolInfo {
                address: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                token0: wmatic(), token1: usdt(), fee: 30,
                name: None, dex_type: DexType::UniswapV2, tick_spacing: None,
                creation_block: 0,
                pool_id: None,
            },
            reserve0: 2_000_000, reserve1: 1_000_000,
        };
        let (shared, other, fee) = v2_reserves(&pool, wmatic(), false).unwrap();
        assert_eq!(shared, 2_000_000);
        assert_eq!(other, 1_000_000);
        assert_eq!(fee, 30);
    }

    // ---- quote_path ----

    #[test]
    fn test_quote_path_v2_v2_profitable() {
        let a = v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000);
        let b = v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 1_000_000, 2_000_000);
        let result = quote_path(&a, &b, wmatic());
        assert!(result.is_some());
        assert!(result.unwrap().profit > 0);
    }

    #[test]
    fn test_quote_path_v2_v2_no_profit_equal_prices() {
        let a = v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 1_000_000);
        let b = v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 1_000_000, 1_000_000);
        assert!(quote_path(&a, &b, wmatic()).is_none());
    }

    #[test]
    fn test_quote_path_v2_v2_low_reserves_still_checks_profit() {
        // Min-reserve filter removed — low reserves may still produce arb if
        // the price gap is large enough to overcome fees
        let a = v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 500, 500);
        let b = v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 500, 500);
        // Equal reserves with same fee → no profit expected (price = 1:1 both pools)
        assert!(quote_path(&a, &b, wmatic()).is_none());
    }

    #[test]
    fn test_quote_path_v3_v3_no_panic() {
        let a = v3_pool(address!("3333333333333333333333333333333333333333"), usdc(), wmatic(), U256::from(1u128 << 96), 0, 1_000_000_000);
        let b = v3_pool(address!("4444444444444444444444444444444444444444"), wmatic(), usdt(), U256::from(2u128 << 96), 10, 1_000_000_000);
        let result = quote_path(&a, &b, wmatic());
        assert!(result.is_none() || result.unwrap().profit > 0);
    }

    #[test]
    fn test_quote_path_v2_v3_mixed() {
        let v2 = v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 1_000_000);
        let v3 = v3_pool(address!("3333333333333333333333333333333333333333"), wmatic(), usdt(), U256::from(1u128 << 96), 0, 1_000_000_000);
        let result = quote_path(&v2, &v3, wmatic());
        assert!(result.is_none() || result.unwrap().profit > 0);
    }

    #[test]
    fn test_quote_path_curve_v2_v2_combo() {
        let mut token_index = HashMap::new();
        token_index.insert(usdc(), 0usize);
        token_index.insert(wmatic(), 1usize);
        let curve = PoolState::Curve(crate::pool::state::CurvePoolState {
            info: PoolInfo {
                address: Address::ZERO, token0: usdc(), token1: wmatic(), fee: 1,
                name: None, dex_type: DexType::Curve, tick_spacing: None,
                creation_block: 0,
                pool_id: None,
            },
            balances: vec![1_000_000, 1_000_000],
            token_index,
            a_coeff: 100,
        });
        let v2 = v2_pool(Address::ZERO, wmatic(), usdt(), 500_000, 1_000_000);
        // Curve-V2 combo should now be supported
        let result = quote_path(&curve, &v2, wmatic());
        // May return None if no profit, but should not panic or skip
        assert!(result.is_none() || result.unwrap().profit > 0);
    }

    // ---- TwoHopArbDetector::detect ----

    fn default_gas_config() -> GasConfig {
        GasConfig::default()
    }

    #[test]
    fn test_detect_finds_arb() {
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000));
        pm.add_pool(v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 1_000_000, 2_000_000));
        let mut detector = TwoHopArbDetector::new(42);
        let opps = detector.detect(&pm, 0, 12345, 50_000_000_000, default_gas_config());
        assert!(!opps.is_empty());
        for opp in &opps {
            assert_eq!(opp.block_number, 42);
            assert_eq!(opp.strategy, Strategy::TwoHopArb);
            assert!(opp.expected_profit > U256::ZERO);
            assert!(opp.gas_cost_wei > 0);
        }
    }

    #[test]
    fn test_detect_empty_pool_manager() {
        let pm = PoolManager::new();
        let mut detector = TwoHopArbDetector::new(1);
        assert!(detector.detect(&pm, 0, 100, 50_000_000_000, default_gas_config()).is_empty());
    }

    #[test]
    fn test_detect_single_pool_no_pairs() {
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000));
        let mut detector = TwoHopArbDetector::new(1);
        assert!(detector.detect(&pm, 0, 100, 50_000_000_000, default_gas_config()).is_empty());
    }
}
