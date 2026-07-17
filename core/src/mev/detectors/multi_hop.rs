//! Multi-hop arbitrage detection — finds profitable swap paths across connected pools (BFS, depth ≤ 4).

use alloy::primitives::{Address, U256};
use crate::types::MevOpportunity;
use crate::pool::math::{constant_product_output_amount, optimal_n_hop_generic, quote_exact_in};
use crate::pool::state::{calldata_gas_estimate, check_dedup_key, PoolManager, PoolState, UniswapV3PoolState};
use crate::pool::math::v3::max_v3_tradeable_amount;
use crate::types::{GasConfig, Strategy};

/// Detects multi-hop arbitrage opportunities across connected pool paths.
///
/// Enumerates pool graphs via BFS (depth ≤ 4) from existing arbitrage pairs,
/// then quotes each path through V2/V3 AMMs. Maintains a per-block dedup set
/// so the same persistent path is not re-reported across multiple transactions.
pub struct MultiHopArbDetector {
    block_number: u64,
    seen: std::collections::HashMap<(Address, Address, Address, Address), (u128, u128)>,
}

impl MultiHopArbDetector {
    /// Create a new detector for the given block.
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            seen: std::collections::HashMap::new(),
        }
    }

    /// Scan all pool paths and emit profitable multi-hop arbitrage opportunities.
    /// Deduplicates per block: each unique (pool_a, pool_b, token_in, token_out) is emitted
    /// at most once per block *unless* pool reserves change by >0.1%, in which case the
    /// dedup is cleared and the opportunity is re-evaluated (H2).
    pub fn detect(
        &mut self,
        pool_manager: &PoolManager,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Vec<MevOpportunity> {
        let max_depth = 4usize;
        let mut opportunities = Vec::new();

        let paths = Self::find_paths(pool_manager, max_depth);

        for path in &paths {
            if let Some(opp) = Self::check_path(
                pool_manager, path,
                self.block_number, tx_index, timestamp,
                base_fee_per_gas, gas_config,
            ) {
                let key = (opp.pool_a, opp.pool_b, opp.token_in, opp.token_out);
                if check_dedup_key(
                    &mut self.seen, &key, pool_manager, opp.pool_a, opp.pool_b,
                ) {
                    opportunities.push(opp);
                }
            }
        }

        opportunities
    }

    /// BFS-limited enumeration of pool paths through the token graph.
    /// Each path is [buy_pool, ..., sell_pool] where adjacent pools share a token.
    pub fn find_paths(pm: &PoolManager, max_depth: usize) -> Vec<Vec<Address>> {
        let mut all_paths = Vec::new();

        // Seed 2-pool paths from existing arbitrage pairs (both directions)
        for &(pool_a, pool_b, _shared) in &pm.arbitrage_pairs() {
            let seed = vec![pool_a, pool_b];
            all_paths.push(seed.clone());
            Self::extend_path(pm, seed, &mut all_paths, max_depth);
            let rev = vec![pool_b, pool_a];
            all_paths.push(rev.clone());
            Self::extend_path(pm, rev, &mut all_paths, max_depth);
        }

        all_paths
    }

    fn extend_path(pm: &PoolManager, path: Vec<Address>, all_paths: &mut Vec<Vec<Address>>, max_depth: usize) {
        if path.len() >= max_depth {
            return;
        }

        let last_pool = match pm.get(&path[path.len() - 1]) {
            Some(p) => p,
            None => return,
        };
        let prev_pool = match pm.get(&path[path.len() - 2]) {
            Some(p) => p,
            None => return,
        };

        // Determine the "forward token" — the token NOT shared with the previous pool
        let forward_token = Self::non_shared_token(last_pool, prev_pool);

        for &next_addr in pm.pools_for_token(&forward_token).into_iter().flatten() {
            if path.contains(&next_addr) {
                continue;
            }
            let mut new_path = path.clone();
            new_path.push(next_addr);
            all_paths.push(new_path.clone());
            Self::extend_path(pm, new_path, all_paths, max_depth);
        }
    }

    /// Given a pool and the previous pool in the path, determine which token
    /// of `pool` is the "forward" side (not shared with `prev`).
    fn non_shared_token(pool: &PoolState, prev: &PoolState) -> Address {
        let info = pool.info();
        let prev_info = prev.info();
        if info.token0 == prev_info.token0 || info.token0 == prev_info.token1 {
            info.token1
        } else {
            info.token0
        }
    }

    fn check_path(
        pm: &PoolManager,
        path: &[Address],
        block_number: u64,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Option<MevOpportunity> {
        if path.len() < 2 {
            return None;
        }

        let pool_a = pm.get(&path[0])?;
        let pool_b = pm.get(&path[path.len() - 1])?;

        // token_in = non-shared side of first pool
        let next = pm.get(&path[1])?;
        let info_a = pool_a.info();
        let info_next = next.info();
        let first_shared = if info_a.token0 == info_next.token0 || info_a.token0 == info_next.token1 {
            info_a.token0
        } else {
            info_a.token1
        };
        let token_in = if info_a.token0 == first_shared {
            info_a.token1
        } else {
            info_a.token0
        };

        // token_out = non-shared side of last pool
        let prev = pm.get(&path[path.len() - 2])?;
        let info_b = pool_b.info();
        let last_shared = if info_b.token0 == prev.info().token0 || info_b.token0 == prev.info().token1 {
            info_b.token0
        } else {
            info_b.token1
        };
        let token_out = if info_b.token0 == last_shared {
            info_b.token1
        } else {
            info_b.token0
        };

        let max_input = Self::pool_max_input(pool_a);

        let quote_fn = |x: u128| -> Option<u128> {
            let mut current = x;
            let mut current_token = token_in;
            for &addr in path {
                let pool = pm.get(&addr)?;
                current = Self::quote_single_pool(pool, current_token, current)?;
                let info = pool.info();
                current_token = if info.token0 == current_token { info.token1 } else { info.token0 };
            }
            Some(current)
        };

        let (input_amount, output_amount) = optimal_n_hop_generic(max_input, &quote_fn)?;

        if output_amount <= input_amount {
            return None;
        }

        let gas_limit = estimate_gas_for_multi_hop(path, pm);
        let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

        let gross_profit = output_amount.saturating_sub(input_amount);
        // Subtract flash loan fee from gross profit
        let flash_fee = gas_config.flash_loan_fee(input_amount);
        let net_profit = gross_profit.saturating_sub(flash_fee);

        // Normalize profit to token_in for non-cyclic paths (H6).
        // Two-hop already handles this; multi-hop was silently dropping non-cyclic paths.
        let (expected_profit, raw_profit) = if token_in == token_out {
            (U256::from(net_profit), None)
        } else {
            let raw = U256::from(net_profit);
            let native_profit = pm.normalize_to_native(token_out, net_profit)
                .or_else(|| {
                    let total_input = input_amount;
                    let total_output = total_input.saturating_add(net_profit);
                    let native_in = pm.normalize_to_native(token_in, total_input)?;
                    let native_out = pm.normalize_to_native(token_out, total_output)?;
                    native_out.checked_sub(native_in)
                })
                .unwrap_or(net_profit);
            (U256::from(native_profit), Some(raw))
        };

        // Compute slippage-adjusted profits
        let eval_raw = |x: u128| -> Option<u128> {
            let mut cur = x;
            let mut cur_token = token_in;
            for &addr in path {
                let pool = pm.get(&addr)?;
                cur = Self::quote_single_pool(pool, cur_token, cur)?;
                let info = pool.info();
                cur_token = if info.token0 == cur_token { info.token1 } else { info.token0 };
            }
            if cur > x { Some(cur - x) } else { None }
        };
        let normalize_slippage = |p: u128| -> Option<U256> {
            if token_in == token_out {
                Some(U256::from(p))
            } else {
                pm.normalize_to_native(token_out, p)
                    .or_else(|| {
                        let native_in = pm.normalize_to_native(token_in, input_amount)?;
                        let native_out = pm.normalize_to_native(token_out, input_amount + p)?;
                        native_out.checked_sub(native_in)
                    })
                    .map(U256::from)
            }
        };
        let p1 = if input_amount > 0 { eval_raw(input_amount.saturating_mul(101) / 100).and_then(normalize_slippage) } else { None };
        let m1 = if input_amount > 0 { eval_raw(input_amount.saturating_mul(99) / 100).and_then(normalize_slippage) } else { None };
        let p2 = if input_amount > 0 { eval_raw(input_amount.saturating_mul(102) / 100).and_then(normalize_slippage) } else { None };
        let m2 = if input_amount > 0 { eval_raw(input_amount.saturating_mul(98) / 100).and_then(normalize_slippage) } else { None };

        Some(MevOpportunity {
            canonical_id: None,
            block_number,
            tx_index,
            strategy: Strategy::MultiHopArb,
            pool_a: path[0],
            pool_b: path[path.len() - 1],
            token_in,
            token_out,
            input_amount: U256::from(input_amount),
            expected_profit,
            raw_profit,
            profit_slippage_p1: p1,
            profit_slippage_m1: m1,
            profit_slippage_p2: p2,
            profit_slippage_m2: m2,
            gas_cost_wei,
            timestamp,
            path: Some(path.to_vec()),
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
        })
    }

    fn pool_max_input(pool: &PoolState) -> u128 {
        match pool {
            PoolState::UniswapV2(v2) => std::cmp::min(v2.reserve0, v2.reserve1),
            PoolState::UniswapV3(v3) => max_v3_tradeable_amount(v3, true)
                .max(max_v3_tradeable_amount(v3, false)),
            PoolState::UniswapV4(v4) => {
                let v3: UniswapV3PoolState = v4.clone().into();
                max_v3_tradeable_amount(&v3, true)
                    .max(max_v3_tradeable_amount(&v3, false))
            }
            PoolState::Curve(c) => {
                c.balances.iter().fold(0u128, |a, &b| a.max(b))
            }
            PoolState::Balancer(b) => {
                b.balances.iter().fold(0u128, |a, &b| a.max(b))
            }
            PoolState::TraderJoeLB(lb) => std::cmp::min(lb.reserve_x, lb.reserve_y),
            PoolState::Pendle(p) => std::cmp::min(p.total_pt, p.total_sy),
            PoolState::Dodo(_) => 0,
        }
    }

    fn quote_single_pool(pool: &PoolState, token_in: Address, amount_in: u128) -> Option<u128> {
        match pool {
            PoolState::UniswapV2(v2) => {
                let (reserve_in, reserve_out) = if v2.info.token0 == token_in {
                    (v2.reserve0, v2.reserve1)
                } else if v2.info.token1 == token_in {
                    (v2.reserve1, v2.reserve0)
                } else {
                    return None;
                };
                constant_product_output_amount(amount_in, reserve_in, reserve_out, v2.info.fee)
            }
            PoolState::Curve(curve) => {
                let token_out = curve.token_index.keys()
                    .filter(|k| **k != token_in)
                    .min()?;
                quote_exact_in(pool, token_in, *token_out, amount_in)
            }
            PoolState::Balancer(bal) => {
                let token_out = *bal.token_index.keys()
                    .filter(|k| **k != token_in)
                    .min()?;
                quote_exact_in(pool, token_in, token_out, amount_in)
            }
            _ => {
                // For V3 and future pool types, use the unified dispatcher
                // which determines token_out from the pool's second token
                let token_out = if pool.info().token0 == token_in {
                    pool.info().token1
                } else if pool.info().token1 == token_in {
                    pool.info().token0
                } else {
                    return None;
                };
                quote_exact_in(pool, token_in, token_out, amount_in)
            }
        }
    }
}

fn estimate_gas_for_multi_hop(path: &[Address], pm: &PoolManager) -> u64 {
    let calldata = calldata_gas_estimate(path.len());
    let mut total = 40_000u64 + calldata;
    for addr in path {
        if let Some(pool) = pm.get(addr) {
            total = total.saturating_add(pool.gas_estimate());
        } else {
            total = total.saturating_add(80_000);
        }
    }
    total
}

