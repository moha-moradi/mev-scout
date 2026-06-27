//! Balancer V2 AMM math: weighted product, StableSwap, and variant dispatch.

use alloy::primitives::{Address, U256};
use crate::pool::state::{BalancerPoolState, BalancerPoolVariant};

/// Dispatch to the correct Balancer quoting formula based on pool variant.
/// Weighted pools use the weighted product formula; Stable pools use StableSwap.
pub fn balancer_quote_exact_in(
    amount_in: u128,
    pool: &BalancerPoolState,
    token_in: Address,
    token_out: Address,
) -> Option<u128> {
    match pool.pool_variant {
        BalancerPoolVariant::Stable | BalancerPoolVariant::ComposableStable => {
            balancer_stable_output_amount(amount_in, pool, token_in, token_out)
        }
        BalancerPoolVariant::Weighted | BalancerPoolVariant::Other => {
            let (reserve_in, reserve_out) = balancer_reserves(pool, token_in, token_out)?;
            let (w_in, w_out) = balancer_weights(pool, token_in, token_out);
            let fee = pool.info.fee;
            balancer_output_amount(amount_in, reserve_in, reserve_out, w_in, w_out, fee)
        }
    }
}

/// Balancer weighted pool output using the weighted product formula.
/// `weights` are in the same order as balances, in basis points (1e18 each).
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
    let fee_factor = 1_000_000u128 - fee as u128;
    let amount_after_fee = amount_in.checked_mul(fee_factor)? / 1_000_000;

    let r_in = U256::from(reserve_in);
    let r_out = U256::from(reserve_out);
    let w_in = U256::from(if weight_in == 0 { 1e18 as u128 } else { weight_in });
    let w_out = U256::from(if weight_out == 0 { 1e18 as u128 } else { weight_out });
    let amount = U256::from(amount_after_fee);

    let numerator = r_in;
    let denominator = r_in + amount;

    if denominator.is_zero() { return None; }

    let ratio_f64 = numerator.as_limbs()[0] as f64 / denominator.as_limbs()[0] as f64;
    let exp = w_in.as_limbs()[0] as f64 / w_out.as_limbs()[0] as f64;
    let reduction = ratio_f64.powf(exp);

    let output_f64 = r_out.as_limbs()[0] as f64 * (1.0 - reduction);
    if output_f64 <= 0.0 { return None; }

    Some(output_f64 as u128)
}

/// Balancer Stable pool output amount using the StableSwap invariant.
///
/// Uses the same Newton's method as Curve's StableSwap, but with the Balancer
/// amplification parameter from `getAmplificationParameter()`.
/// For ComposableStable pools, scaling factors are applied to balances before
/// the invariant computation, and the BPT token is excluded from the math.
pub fn balancer_stable_output_amount(
    amount_in: u128,
    pool: &BalancerPoolState,
    token_in: Address,
    token_out: Address,
) -> Option<u128> {
    let n = pool.balances.len();
    if n < 2 || amount_in == 0 {
        return None;
    }
    let idx_in = *pool.token_index.get(&token_in)?;
    let idx_out = *pool.token_index.get(&token_out)?;

    // For ComposableStable: apply scaling factors and skip BPT index
    let has_scaling = pool.scaling_factors.len() == n && !pool.scaling_factors.is_empty();
    let bpt_idx = pool.bpt_index;

    let scaled_balances: Vec<f64> = pool.balances.iter().enumerate()
        .filter(|(i, _)| bpt_idx.map_or(true, |b| *i != b))
        .map(|(i, &b)| {
            let raw = b as f64;
            if has_scaling {
                raw * pool.scaling_factors[i] as f64 / 1e18f64
            } else {
                raw
            }
        })
        .collect();

    if scaled_balances.len() < 2 {
        return None;
    }

    // Remap indices accounting for BPT removal
    let remap = |orig: usize| -> Option<usize> {
        match bpt_idx {
            Some(bpt) if orig > bpt => Some(orig - 1),
            Some(bpt) if orig == bpt => None,
            _ => Some(orig),
        }
    };
    let si = remap(idx_in)?;
    let so = remap(idx_out)?;

    if si >= scaled_balances.len() || so >= scaled_balances.len() {
        return None;
    }
    if scaled_balances[si] <= 0.0 || scaled_balances[so] <= 0.0 {
        return None;
    }

    let a = pool.amplification.unwrap_or(100) as f64;
    let n_scaled = scaled_balances.len();
    let nn = (n_scaled as f64).powf(n_scaled as f64);
    let fee_factor = 1.0 - (pool.info.fee as f64) / 1_000_000.0;

    // Phase 1: Compute invariant D
    let sum: f64 = scaled_balances.iter().sum();
    let prod: f64 = scaled_balances.iter().product();
    if prod <= 0.0 {
        return None;
    }
    let ann = a * nn;
    let d = newton_stableswap_invariant(n_scaled, ann, sum, prod, sum)?;

    // Phase 2: Apply fee to input
    let x_in_new = scaled_balances[si] + amount_in as f64 * fee_factor;

    // Phase 3: Solve for x_out'
    let sum_others: f64 = scaled_balances.iter().enumerate()
        .filter(|&(i, _)| i != so)
        .map(|(_, &v)| v)
        .sum::<f64>() + (x_in_new - scaled_balances[si]);
    let prod_others: f64 = scaled_balances.iter().enumerate()
        .filter(|&(i, _)| i != so && i != si)
        .map(|(_, &v)| v)
        .product::<f64>() * x_in_new;
    if prod_others <= 0.0 {
        return None;
    }

    let x_out_new = newton_stableswap_output(n_scaled, ann, d, sum_others, prod_others)?;
    let output = scaled_balances[so] - x_out_new;
    if output <= 0.0 { None } else { Some(output as u128) }
}

fn newton_stableswap_invariant(
    n: usize,
    ann: f64,
    sum: f64,
    prod: f64,
    guess: f64,
) -> Option<f64> {
    let nf = n as f64;
    let np1 = (n + 1) as f64;
    let denom = prod * nf.powf(nf);
    if denom <= 0.0 {
        return None;
    }
    let c = ann - 1.0;
    let target = ann * sum;
    let mut d = guess;
    for _ in 0..128 {
        let d_np1 = d.powf(np1);
        let d_n = d.powf(nf);
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

fn newton_stableswap_output(
    n: usize,
    ann: f64,
    d: f64,
    sum_others: f64,
    prod_others: f64,
) -> Option<f64> {
    let nf = n as f64;
    let np1 = (n + 1) as f64;
    let denom = prod_others * nf.powf(nf);
    if denom <= 0.0 {
        return None;
    }
    let k = d.powf(np1) / denom;
    let b = ann * (sum_others - d) + d;

    let disc = b * b + 4.0 * ann * k;
    if disc < 0.0 {
        return None;
    }
    let mut x = (-b + disc.sqrt()) / (2.0 * ann);
    if x <= 0.0 {
        return None;
    }

    for _ in 0..64 {
        let k_over_x = k / x;
        let f = ann * x + b - k_over_x;
        let deriv = ann + k_over_x / x;
        if deriv.abs() < 1e-30 { break; }
        let x_next = x - f / deriv;
        if (x_next - x).abs() <= 0.5 { x = x_next; break; }
        if x_next <= 0.0 { break; }
        x = x_next;
    }

    if x <= 0.0 { None } else { Some(x) }
}

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

/// Extract Balancer pool reserves for a specific token pair.
fn balancer_reserves(
    pool: &BalancerPoolState,
    token_in: Address,
    token_out: Address,
) -> Option<(u128, u128)> {
    let idx_in = *pool.token_index.get(&token_in)?;
    let idx_out = *pool.token_index.get(&token_out)?;
    Some((pool.balances[idx_in], pool.balances[idx_out]))
}
