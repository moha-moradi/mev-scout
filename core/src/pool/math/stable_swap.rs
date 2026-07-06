//! Shared StableSwap Newton's method math used by both Curve and Balancer.

/// Newton's method to find the StableSwap invariant D from N balances.
///
/// Solves: f(D) = D^(n+1) / (nⁿ·P) + (A·nⁿ - 1)·D - A·nⁿ·S = 0
/// where S = sum(balances), P = prod(balances), n = number of tokens.
pub fn newton_stableswap_invariant(
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
pub fn newton_stableswap_output(
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
