//! Catalogue strategy sub-labels for the explorer (plan §5 / P3.*).
//!
//! Prefer `details.tags` over new [`MevKind`] variants (§8.1). Every tag is a
//! Mode-A (or declared-inferred) fingerprint over already-decoded facts;
//! P&L stays on the family basis (`R`/`O`/`F`) of the parent event.

use alloy::primitives::{Address, U256};

use crate::explorer::types::{Amm, RateCacheFact, SwapFact, TransferFact};
use crate::pool::state::pool_types::{is_fee_on_transfer_token, is_rebase_token};

/// Thursday 00:00 UTC epoch length used by Pharaoh / Blackhole ve(3,3).
pub const EPOCH_SECONDS: u64 = 7 * 24 * 3600;
/// Half-window around the epoch boundary for co-occurrence tagging (P3.15).
pub const EPOCH_BOUNDARY_HALF_WINDOW_SECS: u64 = 2 * 3600;
/// Minimum |pool − exchangeRate| / exchangeRate in bps to tag sAVAX rate arb
/// when the on-chain exchange rate is available (plan P3.16).
pub const SAVAX_RATE_DIVERGENCE_BPS: u64 = 5;

/// True when `ts` falls within ±[`EPOCH_BOUNDARY_HALF_WINDOW_SECS`] of a
/// Thursday 00:00 UTC boundary (Unix epoch weeks land on Thursday).
pub fn near_epoch_boundary(ts: u64) -> bool {
    let rem = ts % EPOCH_SECONDS;
    rem <= EPOCH_BOUNDARY_HALF_WINDOW_SECS
        || rem >= EPOCH_SECONDS.saturating_sub(EPOCH_BOUNDARY_HALF_WINDOW_SECS)
}

/// Collect P3 strategy tags for a realized atomic arb route.
pub fn arb_strategy_tags(
    swaps: &[SwapFact],
    opts: &ArbTagOpts<'_>,
) -> Vec<&'static str> {
    let mut tags: Vec<&'static str> = Vec::new();
    if swaps.is_empty() {
        return tags;
    }

    let has_amm = |a: Amm| swaps.iter().any(|s| s.amm == a);
    let cross_venue = {
        let mut keys: Vec<&str> = swaps.iter().map(|s| s.amm.as_str()).collect();
        keys.sort_unstable();
        keys.dedup();
        keys.len() >= 2
    };

    // P3.5 — Curve pool imbalance arb: Curve leg that rebalances via another
    // venue (TokenExchange during peg deviation → close elsewhere). Curve-only
    // cycles are not labeled imbalance.
    if has_amm(Amm::Curve) && cross_venue {
        tags.push("curve_imbalance");
    }

    // P3.2 — Balancer rate-provider staleness (inferred): Balancer leg on a
    // cross-venue arb with no same-block TokenRateCacheUpdated (ComposableStable
    // emits from the pool, not the Vault — any in-block refresh clears the tag).
    if has_amm(Amm::Balancer) && cross_venue && opts.rate_cache_updates.is_empty() {
        tags.push("rate_provider_staleness");
    }

    // P3.12 — FoT / rebase token arb (drift flag secondary; P&L still R).
    let mut fot = false;
    let mut rebase = false;
    for s in swaps {
        for tok in [s.token_in, s.token_out] {
            if tok.is_zero() {
                continue;
            }
            if is_fee_on_transfer_token(&tok) {
                fot = true;
            }
            if is_rebase_token(&tok) {
                rebase = true;
            }
        }
    }
    if fot {
        tags.push("fot_arb");
    }
    if rebase {
        tags.push("rebase_arb");
    }

    // P3.16 — sAVAX rate arb: sAVAX ↔ wrapped-native route; when exchangeRate
    // is known, require pool-implied divergence ≥ SAVAX_RATE_DIVERGENCE_BPS.
    if let Some(savax) = opts.savax {
        let has_savax = swaps
            .iter()
            .any(|s| s.token_in == savax || s.token_out == savax);
        let has_native = swaps.iter().any(|s| {
            s.token_in == opts.wrapped_native || s.token_out == opts.wrapped_native
        });
        if has_savax && has_native {
            let tag = match opts.savax_exchange_rate_wad {
                Some(rate) if !rate.is_zero() => {
                    savax_pool_diverges(swaps, savax, opts.wrapped_native, rate)
                }
                _ => true, // no eth_call rate → route co-presence (mode A surface)
            };
            if tag {
                tags.push("savax_rate_arb");
            }
        }
    }

    // P3.15 — Pharaoh / Blackhole epoch-transition arb.
    if opts.epoch_signal {
        let venue_hit = if opts.epoch_venue_pools.is_empty() {
            // No pool registry: Solidly/LB/V3 legs are the Avalanche surface.
            has_amm(Amm::Solidly) || has_amm(Amm::Lb) || has_amm(Amm::V3)
        } else {
            swaps
                .iter()
                .any(|s| opts.epoch_venue_pools.contains(&s.pool))
        };
        if venue_hit {
            tags.push("epoch_transition");
        }
    }

    // P3.7 — GMX ADL-adjacent arb (co-block EventEmitter signal).
    if opts.gmx_adl_signal {
        tags.push("gmx_adl_arb");
    }

    tags
}

/// True when a sAVAX↔WAVAX leg's implied rate diverges from `exchange_rate_wad`
/// (AVAX per 1e18 sAVAX) by at least [`SAVAX_RATE_DIVERGENCE_BPS`].
fn savax_pool_diverges(
    swaps: &[SwapFact],
    savax: Address,
    wavax: Address,
    exchange_rate_wad: U256,
) -> bool {
    for s in swaps {
        let (shares, avax) = if s.token_in == savax && s.token_out == wavax {
            (s.amount_in, s.amount_out)
        } else if s.token_in == wavax && s.token_out == savax {
            (s.amount_out, s.amount_in)
        } else {
            continue;
        };
        if shares.is_zero() {
            continue;
        }
        // implied = avax * 1e18 / shares
        let implied = avax.saturating_mul(U256::from(10u64.pow(18))) / shares;
        if implied.is_zero() {
            continue;
        }
        let (hi, lo) = if implied > exchange_rate_wad {
            (implied, exchange_rate_wad)
        } else {
            (exchange_rate_wad, implied)
        };
        let bps = (hi - lo)
            .saturating_mul(U256::from(10_000u64))
            / exchange_rate_wad;
        if bps >= U256::from(SAVAX_RATE_DIVERGENCE_BPS) {
            return true;
        }
    }
    false
}

pub struct ArbTagOpts<'a> {
    pub wrapped_native: Address,
    pub savax: Option<Address>,
    /// Benqi `getPooledAvaxByShares(1e18)` at the block (plan P3.16 mode A+B).
    pub savax_exchange_rate_wad: Option<U256>,
    /// True when the block has NotifyReward and/or is near Thursday 00:00 UTC.
    pub epoch_signal: bool,
    pub epoch_venue_pools: &'a [Address],
    pub gmx_adl_signal: bool,
    /// Same-block Balancer TokenRateCacheUpdated facts (plan P3.2).
    pub rate_cache_updates: &'a [RateCacheFact],
}

/// P3.13 — same-tx airdrop/claim mint (Transfer from zero) sold via a swap.
///
/// Returns the claimed token when a zero-from Transfer of `token` is followed
/// by a swap that sells it in the same transaction.
pub fn claim_and_sell_token(
    transfers: &[TransferFact],
    swaps: &[SwapFact],
) -> Option<Address> {
    for t in transfers {
        if !t.from.is_zero() || t.amount.is_zero() || t.token.is_zero() {
            continue;
        }
        let sold = swaps.iter().any(|s| s.token_in == t.token && !s.amount_in.is_zero());
        if sold {
            return Some(t.token);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};
    use crate::explorer::types::LegSource;

    const USDC: Address = address!("4000000000000000000000000000000000000005");
    const WAVAX: Address = address!("b31f66aa3c1e785363f0875a1b74e27b85fd66c7");
    const SAVAX: Address = address!("2b2c81e08f1af8835a78bb2a90ae924ace0ea4be");
    const POOL: Address = address!("3000000000000000000000000000000000000003");
    const POOL_B: Address = address!("3000000000000000000000000000000000000004");

    fn swap(amm: Amm, tin: Address, tout: Address) -> SwapFact {
        SwapFact {
            tx_index: 0,
            log_index: 0,
            pool: POOL,
            amm,
            token_in: tin,
            token_out: tout,
            amount_in: U256::from(100),
            amount_out: U256::from(100),
            token_source: LegSource::Registry,
            tick: None,
            owner: None,
        }
    }

    fn opts() -> ArbTagOpts<'static> {
        ArbTagOpts {
            wrapped_native: WAVAX,
            savax: None,
            savax_exchange_rate_wad: None,
            epoch_signal: false,
            epoch_venue_pools: &[],
            gmx_adl_signal: false,
            rate_cache_updates: &[],
        }
    }

    #[test]
    fn curve_imbalance_requires_cross_venue() {
        let o = opts();
        let tags = arb_strategy_tags(&[swap(Amm::Curve, USDC, WAVAX)], &o);
        assert!(!tags.contains(&"curve_imbalance"));
        let s0 = swap(Amm::Curve, USDC, WAVAX);
        let mut s1 = swap(Amm::V2, WAVAX, USDC);
        s1.pool = POOL_B;
        let tags = arb_strategy_tags(&[s0, s1], &o);
        assert!(tags.contains(&"curve_imbalance"));
    }

    #[test]
    fn balancer_staleness_cleared_by_rate_cache() {
        let mut o = opts();
        let s0 = swap(Amm::Balancer, USDC, WAVAX);
        let mut s1 = swap(Amm::V2, WAVAX, USDC);
        s1.pool = POOL_B;
        let tags = arb_strategy_tags(&[s0.clone(), s1.clone()], &o);
        assert!(tags.contains(&"rate_provider_staleness"));
        let updates = [RateCacheFact {
            tx_index: 0,
            log_index: 0,
            pool: POOL,
            token_index: 0,
            rate: U256::from(1),
        }];
        o.rate_cache_updates = &updates;
        let tags = arb_strategy_tags(&[s0, s1], &o);
        assert!(!tags.contains(&"rate_provider_staleness"));
    }

    #[test]
    fn savax_rate_arb_requires_both_legs() {
        let mut o = opts();
        o.savax = Some(SAVAX);
        let tags = arb_strategy_tags(&[swap(Amm::V2, SAVAX, WAVAX)], &o);
        assert!(tags.contains(&"savax_rate_arb"));
        let tags = arb_strategy_tags(&[swap(Amm::V2, SAVAX, USDC)], &o);
        assert!(!tags.contains(&"savax_rate_arb"));
    }

    #[test]
    fn savax_rate_arb_respects_exchange_rate_divergence() {
        let mut o = opts();
        o.savax = Some(SAVAX);
        // 1.1 AVAX per sAVAX
        o.savax_exchange_rate_wad = Some(U256::from(11u64) * U256::from(10u64.pow(17)));
        let mut s = swap(Amm::V2, SAVAX, WAVAX);
        s.amount_in = U256::from(10u64.pow(18));
        s.amount_out = U256::from(11u64) * U256::from(10u64.pow(17)); // matches rate
        assert!(!arb_strategy_tags(&[s.clone()], &o).contains(&"savax_rate_arb"));
        s.amount_out = U256::from(12u64) * U256::from(10u64.pow(17)); // ~9% high
        assert!(arb_strategy_tags(&[s], &o).contains(&"savax_rate_arb"));
    }

    #[test]
    fn epoch_boundary_thursday_window() {
        // 2024-01-04 00:00:00 UTC = Thursday (1704326400).
        assert!(near_epoch_boundary(1_704_326_400));
        assert!(near_epoch_boundary(1_704_326_400 + 1800));
        assert!(!near_epoch_boundary(1_704_326_400 + 3 * 3600));
    }

    #[test]
    fn claim_and_sell_detects_mint_then_swap() {
        let token = USDC;
        let transfers = vec![TransferFact {
            tx_index: 0,
            log_index: 0,
            token,
            from: Address::ZERO,
            to: address!("1000000000000000000000000000000000000001"),
            amount: U256::from(1000),
        }];
        let swaps = vec![swap(Amm::V2, token, WAVAX)];
        assert_eq!(claim_and_sell_token(&transfers, &swaps), Some(token));
        assert_eq!(claim_and_sell_token(&transfers, &[]), None);
    }
}
