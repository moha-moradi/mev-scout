//! Curve AMM math: StableSwap (V1) and CryptoSwap (V2) quoting functions.

use alloy::primitives::Address;
use crate::pool::state::{CurvePoolState, CurvePoolVariant};

/// Dispatch to the correct Curve quoting formula based on pool variant.
pub fn curve_output_amount(
    amount_in: u128,
    pool: &CurvePoolState,
    token_in: Address,
    token_out: Address,
) -> Option<u128> {
    match pool.pool_variant {
        CurvePoolVariant::Crypto => {
            curve_cryptoswap_output_amount(amount_in, pool, token_in, token_out)
        }
        CurvePoolVariant::Meta => {
            // For metapools, if the output token is a base-pool LP token,
            // we quote as a plain StableSwap step. The base-pool chaining
            // is handled upstream by the calling code.
            curve_stableswap_output_amount(amount_in, pool, token_in, token_out)
        }
        CurvePoolVariant::Plain | CurvePoolVariant::Other => {
            curve_stableswap_output_amount(amount_in, pool, token_in, token_out)
        }
    }
}

/// StableSwap output amount using Newton's method for the invariant D.
///
/// Handles Curve pools with any number of tokens (n >= 2) by computing the
/// generalized StableSwap invariant:
///   A · nⁿ · Σxᵢ + D = A · nⁿ · D + Dⁿ⁺¹ / (nⁿ · Πxᵢ)
///
/// Uses f64 arithmetic — the result is a profit estimate, which does not
/// require exact EVM precision. The Newton iteration converges quickly
/// (typically < 32 steps) to machine epsilon.
pub fn curve_stableswap_output_amount(
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
    let nn = (n as f64).powf(n as f64);
    let fee_factor = 1.0 - (pool.info.fee as f64) / 1_000_000.0;

    // Phase 1: Compute invariant D from all balances (Newton's method)
    let sum: f64 = balances.iter().sum();
    let prod: f64 = balances.iter().product();
    if prod <= 0.0 {
        return None;
    }
    let ann = a * nn;
    let d = newton_stableswap_invariant(n, ann, sum, prod, sum)?;

    // Phase 2: Apply fee to input
    let x_in_new = balances[idx_in] + amount_in as f64 * fee_factor;

    // Phase 3: Solve for x_out' (Newton)
    let sum_others: f64 = balances.iter().enumerate()
        .filter(|&(i, _)| i != idx_out)
        .map(|(_, &v)| v)
        .sum::<f64>() + (x_in_new - balances[idx_in]);
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
/// Solves: f(D) = D^(n+1) / (nⁿ·P) + (A·nⁿ - 1)·D - A·nⁿ·S = 0
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

/// Newton's method to find the new output token balance after a swap.
///
/// Solves: ann·x + ann·(S - D) + D - K/x = 0 where K = Dⁿ⁺¹ / (nⁿ · P)
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

/// CryptoSwap (V2) output amount.
///
/// Implements the Curve CryptoSwap invariant:
///   K₀ = Πxᵢ · N^N / D^N
/// with gamma (price-invariant-convergence) and price_scale per non-first token.
///
/// This is a simplified f64 approximation. The full Solidity implementation
/// uses integer Newton iteration over D, then over y (output balance), and
/// applies a dynamic fee that scales with price deviation.
///
/// Reference: curve-crypto-contract `get_y` / `get_dx`
pub fn curve_cryptoswap_output_amount(
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
    let gamma = pool.gamma.unwrap_or(1) as f64;
    if gamma <= 0.0 {
        return None;
    }
    let nf = n as f64;
    let nn = nf.powf(nf);

    // Phase 1: Compute invariant D using CryptoSwap Newton
    // K₀ = Πxᵢ · N^N / D^N
    // K = K₀ · gamma² / (gamma + 1 - K₀)²  (adjusted with gamma convergence)
    // The invariant: K · D² + (A·nⁿ·gamma) · D - A·nⁿ·gamma · sum = 0
    let sum: f64 = balances.iter().sum();
    let prod: f64 = balances.iter().product();
    if prod <= 0.0 {
        return None;
    }

    let ann = a * nn;
    let d = newton_cryptoswap_invariant(n, ann, gamma, sum, prod, sum)?;

    // Phase 2: Static fee (Tier 1 approximation — see T2.1 for dynamic fee)
    // For CryptoSwap V2, the dynamic fee = fee + (price_deviation * fee_gamma),
    // but for now we use the static fee() value as a conservative approximation.
    let fee_factor = 1.0 - (pool.info.fee as f64) / 1_000_000.0;
    let _x_in_new = balances[idx_in] + amount_in as f64 * fee_factor;

    // Phase 3: Solve for x_out' (Newton over y)
    // Price scale: adjust balances using price_scale before invariant computation.
    // For non-first tokens, balance_i_adj = balance_i * 1e18 / price_scale[i-1].
    let price_scales: Vec<f64> = if pool.price_scale.len() == n - 1 {
        let mut ps = vec![1.0f64; n];
        for i in 1..n {
            let scale = pool.price_scale[i - 1] as f64;
            if scale > 0.0 {
                ps[i] = 1e18f64 / scale;
            }
        }
        ps
    } else {
        vec![1.0f64; n]
    };

    // Adjust balances by price scales
    let adj_balances: Vec<f64> = balances.iter().enumerate()
        .map(|(i, &b)| b * price_scales[i])
        .collect();

    let x_in_adj = adj_balances[idx_in] + amount_in as f64 * fee_factor * price_scales[idx_in];

    let sum_adj_others: f64 = adj_balances.iter().enumerate()
        .filter(|&(i, _)| i != idx_out)
        .map(|(_, &v)| v)
        .sum::<f64>() + (x_in_adj - adj_balances[idx_in]);

    let prod_adj_others: f64 = adj_balances.iter().enumerate()
        .filter(|&(i, _)| i != idx_out && i != idx_in)
        .map(|(_, &v)| v)
        .product::<f64>() * x_in_adj;

    if prod_adj_others <= 0.0 {
        return None;
    }

    let x_out_new_adj = newton_cryptoswap_output(n, ann, gamma, d, sum_adj_others, prod_adj_others)?;

    // Convert back from adjusted to actual balance
    let x_out_new = x_out_new_adj / price_scales[idx_out];
    let output = balances[idx_out] - x_out_new;
    if output <= 0.0 { None } else { Some(output as u128) }
}

/// Newton's method to find the CryptoSwap invariant D.
///
/// The CryptoSwap invariant combines the constant-product K₀ with gamma
/// convergence toward the stable price:
///   K = K₀ · gamma² / (gamma + 1 - K₀)²
///   K · D² + (A·nⁿ·gamma) · D - A·nⁿ·gamma · Σxᵢ = 0
fn newton_cryptoswap_invariant(
    n: usize,
    ann: f64,
    gamma: f64,
    sum: f64,
    prod: f64,
    guess: f64,
) -> Option<f64> {
    let nf = n as f64;
    let nn = nf.powf(nf);
    let gamma2 = gamma * gamma;

    let mut d = guess;
    for _ in 0..128 {
        let d_n = d.powf(nf);
        let k0 = prod * nn / d_n; // K₀ = Πx · N^N / D^N
        if k0 <= 0.0 { break; }
        let k = k0 * gamma2 / ((gamma + 1.0 - k0) * (gamma + 1.0 - k0));
        if k.is_nan() || k.is_infinite() { break; }

        // f(D) = K · D² + (ann·gamma)·D - ann·gamma·sum = 0
        // f'(D) = 2·K·D + ann·gamma
        let f = k * d * d + ann * gamma * d - ann * gamma * sum;
        let deriv = 2.0 * k * d + ann * gamma;
        if deriv.abs() < 1e-30 { break; }
        let d_next = d - f / deriv;
        if (d_next - d).abs() <= 1.0 { d = d_next; break; }
        if d_next <= 0.0 { break; }
        d = d_next;
    }
    if d <= 0.0 { None } else { Some(d) }
}

/// Newton's method to find the new output token balance in a CryptoSwap pool.
///
/// After adjusting balances by price scale, uses the same K(D) formula
/// but solves for the unknown output balance x:
///   K(D) · (x + S)² + ann·gamma·(x + S) - ann·gamma·x - ann·gamma·D = 0
fn newton_cryptoswap_output(
    n: usize,
    ann: f64,
    gamma: f64,
    d: f64,
    sum_others: f64,
    prod_others: f64,
) -> Option<f64> {
    let nf = n as f64;
    let nn = nf.powf(nf);
    let gamma2 = gamma * gamma;
    let d_n = d.powf(nf);

    let k0 = prod_others * nn / d_n;
    if k0 <= 0.0 {
        return None;
    }
    let k = k0 * gamma2 / ((gamma + 1.0 - k0) * (gamma + 1.0 - k0));
    if k.is_nan() || k.is_infinite() {
        return None;
    }

    // f(x) = k·x² + k·(2·S - D)·x + k·S·(S - D) + ann·gamma·(S - D)
    // f'(x) = 2·k·x + k·(2·S - D)
    // Initial guess: x ≈ D / (n · price_scale_factor)
    let mut x = d / nf;
    if x <= 0.0 {
        return None;
    }

    let s = sum_others;
    let b = 2.0 * s - d; // 2S - D
    let c_term = k * s * (s - d) + ann * gamma * (s - d);

    for _ in 0..64 {
        let f = k * x * x + k * b * x + c_term;
        let deriv = 2.0 * k * x + k * b;
        if deriv.abs() < 1e-30 { break; }
        let x_next = x - f / deriv;
        if (x_next - x).abs() <= 0.5 { x = x_next; break; }
        if x_next <= 0.0 { break; }
        x = x_next;
    }

    if x <= 0.0 { None } else { Some(x) }
}
