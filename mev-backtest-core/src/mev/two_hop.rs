use std::cmp;

use alloy::primitives::{address, Address, U256};

use crate::mev::opportunity::MevOpportunity;
use crate::mev::pricing;
use crate::pool::math::{constant_product_output_amount, optimal_two_hop_arb, optimal_two_hop_arb_generic};
use crate::pool::state::{PoolManager, PoolState, UniswapV2PoolState};
use crate::pool::v3_quote::quote_v3_exact_in;
use crate::types::Strategy;

const DEFAULT_GAS_LIMIT: u64 = 200_000;

/// Detects two-hop arbitrage opportunities across V2, V3, and mixed pools.
pub struct TwoHopArbDetector {
    pub min_profit_usd: f64,
    pub gas_limit: u64,
}

impl TwoHopArbDetector {
    pub fn new(min_profit_usd: f64) -> Self {
        TwoHopArbDetector {
            min_profit_usd,
            gas_limit: DEFAULT_GAS_LIMIT,
        }
    }

    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    /// Detect arbitrage opportunities across all pool pairs in the manager.
    pub fn detect(
        &self,
        pool_manager: &PoolManager,
        block_number: u64,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        priority_fee_gwei: f64,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();
        let pairs = pool_manager.arbitrage_pairs();

        for (pool_a, pool_b, shared_token) in &pairs {
            if let Some(opp) = self.check_direction(
                pool_manager, *pool_a, *pool_b, *shared_token,
                block_number, tx_index, timestamp,
                base_fee_per_gas, priority_fee_gwei,
            ) {
                opportunities.push(opp);
            }
            if let Some(opp) = self.check_direction(
                pool_manager, *pool_b, *pool_a, *shared_token,
                block_number, tx_index, timestamp,
                base_fee_per_gas, priority_fee_gwei,
            ) {
                opportunities.push(opp);
            }
        }

        opportunities
    }

    fn check_direction(
        &self,
        pm: &PoolManager,
        buy_pool: Address,
        sell_pool: Address,
        shared_token: Address,
        block_number: u64,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        priority_fee_gwei: f64,
    ) -> Option<MevOpportunity> {
        let pool_a = pm.get(&buy_pool)?;
        let pool_b = pm.get(&sell_pool)?;

        let (token_in, token_out) = arb_tokens(pool_a, pool_b, shared_token)?;

        let result = match (pool_a, pool_b) {
            (PoolState::UniswapV2(a), PoolState::UniswapV2(b)) => {
                let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
                let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
                let min_reserve = 1000u128;
                if r_a_other < min_reserve || r_a_shared < min_reserve
                    || r_b_in < min_reserve || r_b_out < min_reserve
                {
                    return None;
                }
                optimal_two_hop_arb(r_a_other, r_a_shared, fee_a, r_b_in, r_b_out, fee_b)?
            }
            (PoolState::UniswapV3(a), PoolState::UniswapV3(b)) => {
                let zero_a = shared_token == a.info.token1;
                let zero_b = shared_token == b.info.token0;
                let max_input = cmp::max(a.liquidity, b.liquidity);
                let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
                let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
                optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)?
            }
            (PoolState::UniswapV2(a), PoolState::UniswapV3(b)) => {
                let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
                let zero_b = shared_token == b.info.token0;
                let max_input = r_a_other;
                let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
                let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
                optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)?
            }
            (PoolState::UniswapV3(a), PoolState::UniswapV2(b)) => {
                let zero_a = shared_token == a.info.token1;
                let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
                let max_input = r_b_out;
                let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
                let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
                optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)?
            }
            _ => return None,
        };

        if result.profit == 0 {
            return None;
        }

        let profit_u256 = U256::from(result.profit);

        let gas_cost_wei = (self.gas_limit as u128)
            .checked_mul(base_fee_per_gas + (priority_fee_gwei * 1e9) as u128)
            .unwrap_or(u128::MAX);
        let gas_cost_matic = gas_cost_wei as f64 / 1e18;
        let matic_price = pricing::onchain_usd_price(address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270"), pm)
            .unwrap_or_else(pricing::matic_usd_price);
        let gas_cost_usd = gas_cost_matic * matic_price;

        let expected_profit_usd = pricing::raw_amount_to_usd_onchain(token_out, result.profit, pm)
            .unwrap_or_else(|| pricing::raw_amount_to_usd(token_out, result.profit).unwrap_or(0.0));
        let net_profit_usd = expected_profit_usd - gas_cost_usd;

        if net_profit_usd < self.min_profit_usd {
            return None;
        }

        Some(MevOpportunity {
            block_number,
            tx_index,
            strategy: Strategy::TwoHopArb,
            pool_a: buy_pool,
            pool_b: sell_pool,
            token_in,
            token_out,
            input_amount: U256::from(result.input_amount),
            expected_profit: profit_u256,
            expected_profit_usd,
            gas_cost_usd,
            net_profit_usd,
            timestamp,
        })
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
