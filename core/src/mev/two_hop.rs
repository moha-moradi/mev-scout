//! Two-hop arbitrage detection — finds cyclic arbitrage across two connected pools (V2↔V2, V2↔V3, V3↔V3).

use std::cmp;

use alloy::primitives::{Address, U256};

use crate::mev::opportunity::MevOpportunity;
use crate::pool::math::{constant_product_output_amount, optimal_two_hop_arb, optimal_two_hop_arb_generic, TwoHopArbResult};
use crate::pool::state::{BalancerPoolState, CurvePoolState, PoolManager, PoolState, UniswapV2PoolState};
use crate::pool::v3_quote::quote_v3_exact_in;
use crate::types::{GasConfig, Strategy};

/// Detects two-hop arbitrage opportunities across V2, V3, and mixed pools.
///
/// Uses analytical closed-form solutions for V2 pairs and a step-by-step quote
/// engine for V3 pools. Does not require block-by-block state accumulation.
pub struct TwoHopArbDetector;

impl TwoHopArbDetector {
    /// Check all arbitrage pool-pair directions and emit profitable two-hop opportunities.
    pub fn detect(
        pool_manager: &PoolManager,
        block_number: u64,
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
                block_number, tx_index, timestamp,
                base_fee_per_gas, gas_config,
            ) {
                opportunities.push(opp);
            }
            if let Some(opp) = Self::check_direction(
                pool_manager, *pool_b, *pool_a, *shared_token,
                block_number, tx_index, timestamp,
                base_fee_per_gas, gas_config,
            ) {
                opportunities.push(opp);
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

        let gas_cost_wei = gas_config.compute_gas_cost(Strategy::TwoHopArb, base_fee_per_gas, &std::collections::HashMap::new());

        Some(MevOpportunity {
            block_number,
            tx_index,
            strategy: Strategy::TwoHopArb,
            pool_a: buy_pool,
            pool_b: sell_pool,
            token_in,
            token_out,
            input_amount: U256::from(result.input_amount),
            expected_profit: U256::from(result.profit),
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
    match (pool_a, pool_b) {
        (PoolState::UniswapV2(a), PoolState::UniswapV2(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            optimal_two_hop_arb(r_a_other, r_a_shared, fee_a, r_b_in, r_b_out, fee_b)
        }
        (PoolState::UniswapV3(a), PoolState::UniswapV3(b)) => {
            let zero_a = shared_token == a.info.token1;
            let zero_b = shared_token == b.info.token0;
            let max_input = cmp::max(a.liquidity, b.liquidity);
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
            let (b_a_in, b_a_out, fee_a) = curve_reserves(a, shared_token, true)?;
            let (b_b_in, b_b_out, fee_b) = curve_reserves(b, shared_token, false)?;
            let max_input = b_a_in;
            let quote_a = |x: u128| curve_output_amount(x, b_a_in, b_a_out, fee_a);
            let quote_b = |x: u128| curve_output_amount(x, b_b_in, b_b_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::Balancer(b)) => {
            let (b_a_in, b_a_out, fee_a) = balancer_reserves(a, shared_token, true)?;
            let (b_b_in, b_b_out, fee_b) = balancer_reserves(b, shared_token, false)?;
            let max_input = b_a_in;
            let quote_a = |x: u128| balancer_output_amount(x, b_a_in, b_a_out, fee_a);
            let quote_b = |x: u128| balancer_output_amount(x, b_b_in, b_b_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Curve(a), PoolState::UniswapV2(b)) => {
            let (b_a_in, b_a_out, fee_a) = curve_reserves(a, shared_token, true)?;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            let quote_a = |x: u128| curve_output_amount(x, b_a_in, b_a_out, fee_a);
            let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
            optimal_two_hop_arb_generic(b_a_in, &quote_a, &quote_b)
        }
        (PoolState::UniswapV2(a), PoolState::Curve(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let (b_b_in, b_b_out, fee_b) = curve_reserves(b, shared_token, false)?;
            let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
            let quote_b = |x: u128| curve_output_amount(x, b_b_in, b_b_out, fee_b);
            optimal_two_hop_arb_generic(r_a_other, &quote_a, &quote_b)
        }
        (PoolState::Curve(a), PoolState::UniswapV3(b)) => {
            let (b_a_in, b_a_out, fee_a) = curve_reserves(a, shared_token, true)?;
            let zero_b = shared_token == b.info.token0;
            let quote_a = |x: u128| curve_output_amount(x, b_a_in, b_a_out, fee_a);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            optimal_two_hop_arb_generic(b_a_in, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::Curve(b)) => {
            let zero_a = shared_token == a.info.token1;
            let (b_b_in, b_b_out, fee_b) = curve_reserves(b, shared_token, false)?;
            let max_input = a.liquidity;
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| curve_output_amount(x, b_b_in, b_b_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::UniswapV2(b)) => {
            let (b_a_in, b_a_out, fee_a) = balancer_reserves(a, shared_token, true)?;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            let quote_a = |x: u128| balancer_output_amount(x, b_a_in, b_a_out, fee_a);
            let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
            optimal_two_hop_arb_generic(b_a_in, &quote_a, &quote_b)
        }
        (PoolState::UniswapV2(a), PoolState::Balancer(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let (b_b_in, b_b_out, fee_b) = balancer_reserves(b, shared_token, false)?;
            let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
            let quote_b = |x: u128| balancer_output_amount(x, b_b_in, b_b_out, fee_b);
            optimal_two_hop_arb_generic(r_a_other, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::UniswapV3(b)) => {
            let (b_a_in, b_a_out, fee_a) = balancer_reserves(a, shared_token, true)?;
            let zero_b = shared_token == b.info.token0;
            let quote_a = |x: u128| balancer_output_amount(x, b_a_in, b_a_out, fee_a);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            optimal_two_hop_arb_generic(b_a_in, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::Balancer(b)) => {
            let zero_a = shared_token == a.info.token1;
            let (b_b_in, b_b_out, fee_b) = balancer_reserves(b, shared_token, false)?;
            let max_input = a.liquidity;
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| balancer_output_amount(x, b_b_in, b_b_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        // Unsupported type combinations
        _ => None,
    }
}

/// Extract the token_in (spent) and token_out (received) for a two-hop arb
/// given two pools that share a common token.
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
        return None;
    };
    let token_out = if info_b.token0 == shared_token {
        info_b.token1
    } else if info_b.token1 == shared_token {
        info_b.token0
    } else {
        return None;
    };
    Some((token_in, token_out))
}

/// Simplified Curve 2-token output approximation.
/// Uses constant-product approximation (reasonable for stablecoin pools near 1:1).
pub fn curve_output_amount(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee: u32,
) -> Option<u128> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return None;
    }
    let fee_factor = 10000u128 - fee as u128;
    let amount_after_fee = amount_in.checked_mul(fee_factor)? / 10000;
    let numerator = amount_after_fee.checked_mul(reserve_out)?;
    let denominator = reserve_in.checked_add(amount_after_fee)?;
    let output = numerator / denominator;
    if output == 0 { None } else { Some(output) }
}

/// Simplified Balancer weighted pool output approximation.
/// Uses constant-product formula (reasonable for equal-weight pools).
pub fn balancer_output_amount(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee: u32,
) -> Option<u128> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return None;
    }
    let fee_factor = 10000u128 - fee as u128;
    let amount_after_fee = amount_in.checked_mul(fee_factor)? / 10000;
    let numerator = amount_after_fee.checked_mul(reserve_out)?;
    let denominator = reserve_in.checked_add(amount_after_fee)?;
    let output = numerator / denominator;
    if output == 0 { None } else { Some(output) }
}

/// Extract Curve pool reserves for a given direction relative to `shared_token`.
/// `buy_side = true` → we give the other token, receive shared_token.
fn curve_reserves(
    pool: &CurvePoolState,
    shared_token: Address,
    buy_side: bool,
) -> Option<(u128, u128, u32)> {
    let fee = pool.info.fee;
    let idx_shared = *pool.token_index.get(&shared_token)?;
    let idx_other = pool.token_index.iter()
        .find(|(k, _)| **k != shared_token)
        .map(|(_, v)| *v)?;
    if buy_side {
        // We give the other token, receive shared_token
        Some((pool.balances[idx_other], pool.balances[idx_shared], fee))
    } else {
        // We give shared_token, receive the other token
        Some((pool.balances[idx_shared], pool.balances[idx_other], fee))
    }
}

/// Extract Balancer pool reserves for a given direction relative to `shared_token`.
fn balancer_reserves(
    pool: &BalancerPoolState,
    shared_token: Address,
    buy_side: bool,
) -> Option<(u128, u128, u32)> {
    let fee = pool.info.fee;
    let idx_shared = *pool.token_index.get(&shared_token)?;
    let idx_other = pool.token_index.iter()
        .find(|(k, _)| **k != shared_token)
        .map(|(_, v)| *v)?;
    if buy_side {
        Some((pool.balances[idx_other], pool.balances[idx_shared], fee))
    } else {
        Some((pool.balances[idx_shared], pool.balances[idx_other], fee))
    }
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
        let opps = TwoHopArbDetector::detect(&pm, 42, 0, 12345, 50_000_000_000, default_gas_config());
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
        assert!(TwoHopArbDetector::detect(&pm, 1, 0, 100, 50_000_000_000, default_gas_config()).is_empty());
    }

    #[test]
    fn test_detect_single_pool_no_pairs() {
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000));
        assert!(TwoHopArbDetector::detect(&pm, 1, 0, 100, 50_000_000_000, default_gas_config()).is_empty());
    }
}
