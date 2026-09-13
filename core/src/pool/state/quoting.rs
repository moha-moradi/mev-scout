//! DEX-agnostic quoting dispatch on [`PoolState`].
//!
//! Two-hop detection used to enumerate every `(PoolState, PoolState)` pair in
//! a 14+-arm combination matrix; adding a DEX meant adding new match arms in
//! the detector. These methods are the single registry instead: quoting,
//! input bounds, tick-band breakpoints and direction-aware gas routing all
//! dispatch per variant here and delegate to the per-DEX math modules.
//! Adding a DEX touches one math file plus one arm in each method below.

use super::pool_types::PoolState;
use crate::pool::math::quote_exact_in;
use crate::pool::math::v3::{max_v3_tradeable_amount, v3_breakpoints};
use alloy::primitives::Address;

/// How a pool must be optimized inside a two-hop path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteKind {
    /// Constant-product leg with explicit reserves — analytic closed form.
    ConstantProduct,
    /// Concentrated-liquidity tick-band quoting — segmented optimizer.
    Piecewise,
    /// Concave pool-invariant quoting (Curve/Balancer) — generic optimizer.
    Generic,
    /// No two-hop quoting support.
    Unsupported,
}

impl PoolState {
    /// Exact quote of `amount_in` of `token_in` into `token_out`.
    ///
    /// Central routing registry: delegates to the canonical
    /// [`quote_exact_in`] dispatcher in `pool::math::core`.
    pub fn quote_dir(
        &self,
        amount_in: u128,
        token_in: Address,
        token_out: Address,
    ) -> Option<u128> {
        quote_exact_in(self, token_in, token_out, amount_in)
    }

    /// Two-hop optimizer family for this pool.
    pub fn quote_kind(&self) -> QuoteKind {
        match self {
            PoolState::UniswapV2(_) => QuoteKind::ConstantProduct,
            PoolState::UniswapV3(_) | PoolState::UniswapV4(_) | PoolState::PancakeInfinity(_) => {
                QuoteKind::Piecewise
            }
            PoolState::Curve(_) | PoolState::Balancer(_) => QuoteKind::Generic,
            PoolState::TraderJoeLB(_)
            | PoolState::Pendle(_)
            | PoolState::Metric(_)
            | PoolState::Fluid(_) => QuoteKind::Unsupported,
        }
    }

    /// Largest input the pool can absorb on the `token_in` side.
    ///
    /// V3-family pools are bounded by their in-range liquidity; reserve-style
    /// pools by the stored reserve/balance of `token_in`.
    pub fn max_input(&self, token_in: Address) -> Option<u128> {
        match self {
            PoolState::UniswapV2(p) => reserve_of_other(
                p.info.token0,
                p.info.token1,
                p.reserve0,
                p.reserve1,
                token_in,
            ),
            PoolState::UniswapV3(p) | PoolState::UniswapV4(p) | PoolState::PancakeInfinity(p) => {
                Some(max_v3_tradeable_amount(p, p.info.token0 == token_in))
            }
            PoolState::Curve(p) => p.balances.get(*p.token_index.get(&token_in)?).copied(),
            PoolState::Balancer(p) => p.balances.get(*p.token_index.get(&token_in)?).copied(),
            PoolState::TraderJoeLB(p) => reserve_of_other(
                p.info.token0,
                p.info.token1,
                p.reserve_x,
                p.reserve_y,
                token_in,
            ),
            PoolState::Pendle(p) => reserve_of_other(
                p.info.token0,
                p.info.token1,
                p.total_pt,
                p.total_sy,
                token_in,
            ),
            PoolState::Metric(_) | PoolState::Fluid(_) => None,
        }
    }

    /// Amount of `token_out` the pool can return on its sell side (reserve of
    /// the output token). Used to bound a two-hop input when this pool is the
    /// non-piecewise second leg and the first leg is piecewise.
    pub fn sell_reserve(&self, token_out: Address) -> Option<u128> {
        match self {
            PoolState::UniswapV2(p) => reserve_of_other(
                p.info.token0,
                p.info.token1,
                p.reserve0,
                p.reserve1,
                token_out,
            ),
            PoolState::TraderJoeLB(p) => reserve_of_other(
                p.info.token0,
                p.info.token1,
                p.reserve_x,
                p.reserve_y,
                token_out,
            ),
            PoolState::Pendle(p) => reserve_of_other(
                p.info.token0,
                p.info.token1,
                p.total_pt,
                p.total_sy,
                token_out,
            ),
            PoolState::Curve(p) => p.balances.get(*p.token_index.get(&token_out)?).copied(),
            PoolState::Balancer(p) => p.balances.get(*p.token_index.get(&token_out)?).copied(),
            _ => None,
        }
    }

    /// Reserve pair oriented across `shared_token`: `(reserve_in, reserve_out)`.
    ///
    /// `buy_side = true` means we spend the other token to receive
    /// `shared_token`; `false` means we give `shared_token` and receive the
    /// other token. Multi-token and concentrated-liquidity pools return `None`
    /// (quoted through [`PoolState::quote_dir`] instead of closed form).
    pub fn reserve_pair(&self, shared_token: Address, buy_side: bool) -> Option<(u128, u128)> {
        let oriented = |tokens: [(Address, u128); 2]| -> Option<(u128, u128)> {
            let other = tokens
                .iter()
                .find(|(t, _)| *t != shared_token)
                .map(|(_, r)| *r)?;
            let shared = tokens
                .iter()
                .find(|(t, _)| *t == shared_token)
                .map(|(_, r)| *r)?;
            if buy_side {
                Some((other, shared))
            } else {
                Some((shared, other))
            }
        };
        match self {
            PoolState::UniswapV2(p) => {
                oriented([(p.info.token0, p.reserve0), (p.info.token1, p.reserve1)])
            }
            PoolState::TraderJoeLB(p) => {
                oriented([(p.info.token0, p.reserve_x), (p.info.token1, p.reserve_y)])
            }
            PoolState::Pendle(p) => {
                oriented([(p.info.token0, p.total_pt), (p.info.token1, p.total_sy)])
            }
            _ => None,
        }
    }

    /// Tertiary tick-band breakpoints for the piecewise optimizer, empty for
    /// non-concentrated-liquidity pools.
    pub fn seg_breakpoints(
        &self,
        max_input: u128,
        token_in: Address,
        max_points: usize,
    ) -> Vec<u128> {
        match self {
            PoolState::UniswapV3(p) | PoolState::UniswapV4(p) | PoolState::PancakeInfinity(p) => {
                v3_breakpoints(p, p.info.token0 == token_in, max_input, max_points)
            }
            _ => Vec::new(),
        }
    }

    /// Fee denominator of this pool's fee tier (`10_000` bps or `1_000_000` ppm).
    pub fn fee_denom(&self) -> u64 {
        self.info().fee_tier().fraction().1 as u64
    }

    /// Canonical token pair `(token0, token1)` in address order.
    pub fn token_pair(&self) -> (Address, Address) {
        (self.info().token0, self.info().token1)
    }

    /// Direction-aware gas estimate for a swap spending `token_in`.
    ///
    /// V3-family pools use per-direction tick-crossing estimation; reserve and
    /// invariant pools fall back to the flat per-type gas benchmark.
    pub fn swap_gas_estimate(&self, token_in: Address) -> u64 {
        use crate::pool::math::v3::estimate_v3_swap_gas;
        match self {
            PoolState::UniswapV3(p) | PoolState::UniswapV4(p) | PoolState::PancakeInfinity(p) => {
                estimate_v3_swap_gas(p, p.info.token0 == token_in)
            }
            other => other.gas_estimate(),
        }
    }
}

/// Reserve of `token` among the pool's two-token pair; `None` if absent.
fn reserve_of_other(
    token0: Address,
    token1: Address,
    reserve0: u128,
    reserve1: u128,
    token: Address,
) -> Option<u128> {
    if token0 == token {
        Some(reserve0)
    } else if token1 == token {
        Some(reserve1)
    } else {
        None
    }
}
