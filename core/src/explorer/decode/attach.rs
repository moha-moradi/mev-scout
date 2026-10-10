//! Transfer-pairing token attachment for swap facts.
use alloy::primitives::Address;

use crate::explorer::types::{Amm, LegSource, SwapFact, TransferFact};

use super::sentinels::is_unresolved_token;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SideHow {
    Registry,
    Transfer,
    Proximity,
}

fn combine_leg_source(token_in: Option<SideHow>, token_out: Option<SideHow>) -> LegSource {
    match (token_in, token_out) {
        (Some(SideHow::Proximity), _) | (_, Some(SideHow::Proximity)) | (None, _) | (_, None) => {
            LegSource::Proximity
        }
        (Some(SideHow::Transfer), _) | (_, Some(SideHow::Transfer)) => LegSource::Transfer,
        (Some(SideHow::Registry), Some(SideHow::Registry)) => LegSource::Registry,
    }
}

/// Resolve swap token directions from the tx's transfer stream.
///
/// Resolution order:
///
/// 1. Pool registry (`pool_tokens`) maps token0/token1 sentinels. Provenance
///    is [`LegSource::Registry`].
/// 2. Nearest before/after `Transfer` with the pool as counterparty.
///    Provenance is [`LegSource::Transfer`].
/// 3. A ±24-log window when a side is still unresolved. Provenance is
///    [`LegSource::Proximity`], and either side resolved this way taints the leg.
///
/// Balancer already carries topic tokens and skips pairing; it stays
/// `Proximity` because those tokens were not bound by (1) or (2).
///
/// A leg is `Registry` only when both sides came from the registry, and
/// `Transfer` when every resolved side is registry or a strict before/after
/// transfer. Anything else stays `Proximity`.
pub fn attach_swap_tokens(
    swaps: &mut [SwapFact],
    transfers: &[TransferFact],
    pool_tokens: &std::collections::HashMap<Address, (Address, Address)>,
) {
    for s in swaps.iter_mut() {
        // Balancer already carries explicit tokens from topics.
        if s.amm == Amm::Balancer {
            s.token_source = LegSource::Proximity;
            continue;
        }

        let mut in_how = if is_unresolved_token(s.token_in) {
            None
        } else {
            Some(SideHow::Proximity)
        };
        let mut out_how = if is_unresolved_token(s.token_out) {
            None
        } else {
            Some(SideHow::Proximity)
        };

        // 1) Registry resolution (authoritative for known pools).
        if let Some(&(t0, t1)) = pool_tokens.get(&s.pool) {
            let in_before = s.token_in;
            let out_before = s.token_out;
            if s.token_in == super::sentinels::TOKEN0_SENTINEL {
                s.token_in = t0;
            } else if s.token_in == super::sentinels::TOKEN1_SENTINEL {
                s.token_in = t1;
            }
            if s.token_out == super::sentinels::TOKEN0_SENTINEL {
                s.token_out = t0;
            } else if s.token_out == super::sentinels::TOKEN1_SENTINEL {
                s.token_out = t1;
            }
            if is_unresolved_token(s.token_in) && !is_unresolved_token(s.token_out) {
                s.token_in = if s.token_out == t0 { t1 } else { t0 };
            }
            if is_unresolved_token(s.token_out) && !is_unresolved_token(s.token_in) {
                s.token_out = if s.token_in == t0 { t1 } else { t0 };
            }
            if s.token_in != in_before && !is_unresolved_token(s.token_in) {
                in_how = Some(SideHow::Registry);
            }
            if s.token_out != out_before && !is_unresolved_token(s.token_out) {
                out_how = Some(SideHow::Registry);
            }
        }

        // 2) Transfer pairing fallback for any still-unresolved direction.
        let mut token_in = Address::ZERO;
        let mut token_out = Address::ZERO;
        let mut best_in: Option<i64> = None;
        let mut best_out: Option<i64> = None;
        // Flow-ownership: the funder of the input leg is the address
        // that transferred the input token into the pool right before the swap.
        let mut owner: Option<Address> = None;
        for t in transfers {
            let dist = t.log_index as i64 - s.log_index as i64;
            if dist < 0 && t.to == s.pool {
                // nearest before
                match best_in {
                    Some(d) if d <= dist.abs() => {}
                    _ => {
                        best_in = Some(dist.abs());
                        token_in = t.token;
                        owner = Some(t.from);
                    }
                }
            } else if dist > 0 && t.from == s.pool {
                match best_out {
                    Some(d) if d <= dist.abs() => {}
                    _ => {
                        best_out = Some(dist.abs());
                        token_out = t.token;
                    }
                }
            }
        }
        if s.owner.is_none() {
            s.owner = owner;
        }
        if is_unresolved_token(s.token_in) && !token_in.is_zero() {
            s.token_in = token_in;
            in_how = Some(SideHow::Transfer);
        }
        if is_unresolved_token(s.token_out) && !token_out.is_zero() {
            s.token_out = token_out;
            out_how = Some(SideHow::Transfer);
        }

        // Fallback: router/hop layouts sometimes emit the pool→recipient
        // transfer slightly before the next Swap log. Accept nearest pool
        // outflow within a short window when still unresolved.
        if is_unresolved_token(s.token_out) {
            let mut best: Option<(i64, Address)> = None;
            for t in transfers {
                let dist = (t.log_index as i64 - s.log_index as i64).abs();
                if dist == 0 || dist > 24 {
                    continue;
                }
                if t.from != s.pool {
                    continue;
                }
                if t.token.is_zero() || t.token == s.token_in {
                    continue;
                }
                match best {
                    Some((d, _)) if d <= dist => {}
                    _ => best = Some((dist, t.token)),
                }
            }
            if let Some((_, tok)) = best {
                s.token_out = tok;
                out_how = Some(SideHow::Proximity);
            }
        }
        if is_unresolved_token(s.token_in) && !s.token_out.is_zero() {
            let mut best: Option<(i64, Address)> = None;
            for t in transfers {
                let dist = (t.log_index as i64 - s.log_index as i64).abs();
                if dist == 0 || dist > 24 {
                    continue;
                }
                if t.to != s.pool {
                    continue;
                }
                if t.token.is_zero() || t.token == s.token_out {
                    continue;
                }
                match best {
                    Some((d, _)) if d <= dist => {}
                    _ => best = Some((dist, t.token)),
                }
            }
            if let Some((_, tok)) = best {
                s.token_in = tok;
                in_how = Some(SideHow::Proximity);
            }
        }

        s.token_source = combine_leg_source(in_how, out_how);
    }
}

