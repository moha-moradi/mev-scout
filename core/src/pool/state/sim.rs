//! Post-swap pool state for paper simulation (liquidity consumption).
//!
//! [`quote_dir`](super::PoolState::quote_dir) returns only the output amount.
//! These helpers also return the reserves / tick state after the swap so a
//! subsequent candidate can be re-quoted against consumed liquidity.
use alloy::primitives::{Address, U256};

use crate::pool::math::constant_product_output_amount;
use crate::pool::math::v3::{quote_v3_exact_in_post, V3Direction};

use super::pool_types::PoolState;

/// Post-swap state for pool types we can simulate under `--sim state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolPostState {
    V2 {
        reserve0: u128,
        reserve1: u128,
    },
    /// Shared by UniswapV3 / UniswapV4 / PancakeInfinity.
    V3Family {
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: u128,
    },
}

/// Exact-in quote that also yields the post-swap pool state.
///
/// Returns `None` for unsupported / Generic pool types (Curve, Balancer, …)
/// and for failed quotes — callers map that to `SimPathUnavailable`.
pub fn quote_swap_post(
    pool: &PoolState,
    token_in: Address,
    amount_in: u128,
    token_out: Address,
) -> Option<(u128, PoolPostState)> {
    if amount_in == 0 {
        return None;
    }
    match pool {
        PoolState::UniswapV2(v2) => {
            let zero_for_one = if v2.info.token0 == token_in && v2.info.token1 == token_out {
                true
            } else if v2.info.token1 == token_in && v2.info.token0 == token_out {
                false
            } else {
                return None;
            };
            let (reserve_in, reserve_out) = if zero_for_one {
                (v2.reserve0, v2.reserve1)
            } else {
                (v2.reserve1, v2.reserve0)
            };
            let amount_out = constant_product_output_amount(
                amount_in,
                reserve_in,
                reserve_out,
                v2.info.fee_tier(),
            )?;
            // Full amount_in is added to reserves (fee is implicit in amount_out).
            let (reserve0, reserve1) = if zero_for_one {
                (
                    v2.reserve0.saturating_add(amount_in),
                    v2.reserve1.saturating_sub(amount_out),
                )
            } else {
                (
                    v2.reserve0.saturating_sub(amount_out),
                    v2.reserve1.saturating_add(amount_in),
                )
            };
            Some((amount_out, PoolPostState::V2 { reserve0, reserve1 }))
        }
        PoolState::UniswapV3(v3) | PoolState::UniswapV4(v3) | PoolState::PancakeInfinity(v3) => {
            let direction = V3Direction::for_input_token(token_in, v3.info.token0);
            if direction == V3Direction::OneForZero && v3.info.token1 != token_in {
                return None;
            }
            let expected_out = if direction == V3Direction::ZeroForOne {
                v3.info.token1
            } else {
                v3.info.token0
            };
            if expected_out != token_out {
                return None;
            }
            let post = quote_v3_exact_in_post(v3, amount_in, direction)?;
            Some((
                post.amount_out,
                PoolPostState::V3Family {
                    sqrt_price_x96: post.sqrt_price_x96,
                    tick: post.tick,
                    liquidity: post.liquidity,
                },
            ))
        }
        PoolState::Curve(_)
        | PoolState::Balancer(_)
        | PoolState::TraderJoeLB(_)
        | PoolState::Pendle(_)
        | PoolState::Metric(_)
        | PoolState::Fluid(_) => None,
    }
}

/// Apply a [`PoolPostState`] onto a mutable pool (same address / variant).
pub fn apply_post_state(pool: &mut PoolState, post: &PoolPostState) {
    match (pool, post) {
        (PoolState::UniswapV2(v2), PoolPostState::V2 { reserve0, reserve1 }) => {
            v2.reserve0 = *reserve0;
            v2.reserve1 = *reserve1;
        }
        (
            PoolState::UniswapV3(v3) | PoolState::UniswapV4(v3) | PoolState::PancakeInfinity(v3),
            PoolPostState::V3Family {
                sqrt_price_x96,
                tick,
                liquidity,
            },
        ) => {
            v3.sqrt_price_x96 = *sqrt_price_x96;
            v3.tick = *tick;
            v3.liquidity = *liquidity;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex_type::DexType;
    use crate::pool::state::{PoolInfo, PoolManager, UniswapV2PoolState};
    use alloy::primitives::address;

    fn v2_pool(addr: Address, token0: Address, token1: Address, r0: u128, r1: u128) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
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

    #[test]
    fn quote_swap_post_matches_quote_dir() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pool = v2_pool(
            address!("0x00000000000000000000000000000000000000aa"),
            t0,
            t1,
            1_000_000,
            2_000_000,
        );
        let amount_in = 10_000u128;
        let via_dir = pool.quote_dir(amount_in, t0, t1).unwrap();
        let (via_post, _) = quote_swap_post(&pool, t0, amount_in, t1).unwrap();
        assert_eq!(via_dir, via_post);
    }

    #[test]
    fn second_identical_v2_swap_yields_less() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let mut pool = v2_pool(
            address!("0x00000000000000000000000000000000000000aa"),
            t0,
            t1,
            1_000_000,
            2_000_000,
        );
        let amount_in = 50_000u128;
        let (out1, post) = quote_swap_post(&pool, t0, amount_in, t1).unwrap();
        apply_post_state(&mut pool, &post);
        let (out2, _) = quote_swap_post(&pool, t0, amount_in, t1).unwrap();
        assert!(out2 < out1, "out1={out1} out2={out2}");
    }

    #[test]
    fn curve_returns_none() {
        use crate::pool::state::{CurvePoolState, CurvePoolVariant};
        use std::collections::HashMap;
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let mut token_index = HashMap::new();
        token_index.insert(t0, 0usize);
        token_index.insert(t1, 1usize);
        let curve = PoolState::Curve(CurvePoolState {
            info: PoolInfo {
                address: address!("0x00000000000000000000000000000000000000cc"),
                token0: t0,
                token1: t1,
                fee: 4,
                dex_type: DexType::Curve,
                ..Default::default()
            },
            balances: vec![1_000_000, 1_000_000],
            token_index,
            a_coeff: 100,
            pool_variant: CurvePoolVariant::Plain,
            gamma: None,
            price_scale: vec![],
            base_pool: None,
        });
        assert!(quote_swap_post(&curve, t0, 1_000, t1).is_none());
    }

    #[test]
    fn subset_leaves_source_untouched() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let a = address!("0x00000000000000000000000000000000000000aa");
        let b = address!("0x00000000000000000000000000000000000000bb");
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(a, t0, t1, 1_000_000, 2_000_000));
        pm.add_pool(v2_pool(b, t0, t1, 3_000_000, 4_000_000));
        let before_r0 = match pm.get(&a).unwrap() {
            PoolState::UniswapV2(s) => s.reserve0,
            _ => panic!("expected v2"),
        };
        let mut sub = pm.subset(&[a]);
        assert!(sub.get(&a).is_some());
        assert!(sub.get(&b).is_none());
        if let Some(PoolState::UniswapV2(s)) = sub.get_mut(&a) {
            s.reserve0 = 1;
        }
        let after_r0 = match pm.get(&a).unwrap() {
            PoolState::UniswapV2(s) => s.reserve0,
            _ => panic!("expected v2"),
        };
        assert_eq!(after_r0, before_r0);
    }
}
