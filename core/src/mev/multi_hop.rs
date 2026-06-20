//! Multi-hop arbitrage detection — finds profitable swap paths across connected pools (BFS, depth ≤ 4).

use alloy::primitives::{Address, U256};
use crate::mev::opportunity::MevOpportunity;
use crate::pool::math::{constant_product_output_amount, optimal_n_hop_generic};
use crate::pool::state::{calldata_gas_estimate, PoolManager, PoolState};
use crate::pool::v3_quote::{quote_v3_exact_in, max_v3_tradeable_amount};
use crate::mev::two_hop::{curve_output_amount, balancer_output_amount};
use crate::types::{GasConfig, Strategy};

/// Detects multi-hop arbitrage opportunities across connected pool paths.
///
/// Enumerates pool graphs via BFS (depth ≤ 4) from existing arbitrage pairs,
/// then quotes each path through V2/V3 AMMs. Maintains a per-block dedup set
/// so the same persistent path is not re-reported across multiple transactions.
pub struct MultiHopArbDetector {
    block_number: u64,
    seen: std::collections::HashSet<(Address, Address, Address, Address)>,
}

impl MultiHopArbDetector {
    /// Create a new detector for the given block.
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            seen: std::collections::HashSet::new(),
        }
    }

    /// Scan all pool paths and emit profitable multi-hop arbitrage opportunities.
    /// Deduplicates per block: each unique (pool_a, pool_b, token_in, token_out) is emitted
    /// at most once, preventing the same persistent arb gap from being re-reported across
    /// multiple transactions.
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
                if self.seen.insert(key) {
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

        // Only emit cyclic paths where we start and end in the same token.
        // Non-cyclic paths (e.g. USDC→WMATIC→USDT) compare apples-to-oranges.
        if token_in != token_out {
            return None;
        }

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

        // Compute slippage-adjusted profits
        let eval_input = |x: u128| -> Option<U256> {
            let mut cur = x;
            let mut cur_token = token_in;
            for &addr in path {
                let pool = pm.get(&addr)?;
                cur = Self::quote_single_pool(pool, cur_token, cur)?;
                let info = pool.info();
                cur_token = if info.token0 == cur_token { info.token1 } else { info.token0 };
            }
            if cur > x { Some(U256::from(cur - x)) } else { None }
        };
        let p1 = if input_amount > 0 { eval_input(input_amount.saturating_mul(101) / 100) } else { None };
        let m1 = if input_amount > 0 { eval_input(input_amount.saturating_mul(99) / 100) } else { None };
        let p2 = if input_amount > 0 { eval_input(input_amount.saturating_mul(102) / 100) } else { None };
        let m2 = if input_amount > 0 { eval_input(input_amount.saturating_mul(98) / 100) } else { None };

        Some(MevOpportunity {
            block_number,
            tx_index,
            strategy: Strategy::MultiHopArb,
            pool_a: path[0],
            pool_b: path[path.len() - 1],
            token_in,
            token_out,
            input_amount: U256::from(input_amount),
            expected_profit: U256::from(net_profit),
            raw_profit: None,
            profit_slippage_p1: p1,
            profit_slippage_m1: m1,
            profit_slippage_p2: p2,
            profit_slippage_m2: m2,
            pga_adjusted_profit: None,
            gas_cost_wei,
            timestamp,
            path: Some(path.to_vec()),
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
        })
    }

    fn pool_max_input(pool: &PoolState) -> u128 {
        match pool {
            PoolState::UniswapV2(v2) => std::cmp::min(v2.reserve0, v2.reserve1),
            PoolState::UniswapV3(v3) => max_v3_tradeable_amount(v3, true)
                .max(max_v3_tradeable_amount(v3, false)),
            PoolState::Curve(c) => {
                c.balances.iter().fold(0u128, |a, &b| a.max(b))
            }
            PoolState::Balancer(b) => {
                b.balances.iter().fold(0u128, |a, &b| a.max(b))
            }
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
            PoolState::UniswapV3(v3) => {
                let zero_for_one = v3.info.token0 == token_in;
                if !zero_for_one && v3.info.token1 != token_in {
                    return None;
                }
                quote_v3_exact_in(v3, amount_in, zero_for_one)
            }
            PoolState::Curve(curve) => {
                let idx_in = *curve.token_index.get(&token_in)?;
                let idx_out = curve.token_index.iter()
                    .find(|(k, _)| **k != token_in)
                    .map(|(_, v)| *v)?;
                let balance_in = curve.balances[idx_in];
                let balance_out = curve.balances[idx_out];
                curve_output_amount(amount_in, balance_in, balance_out, curve.info.fee, curve.a_coeff)
            }
            PoolState::Balancer(bal) => {
                let idx_in = *bal.token_index.get(&token_in)?;
                let idx_out = bal.token_index.iter()
                    .find(|(k, _)| **k != token_in)
                    .map(|(_, v)| *v)?;
                let balance_in = bal.balances[idx_in];
                let balance_out = bal.balances[idx_out];
                let w_in = bal.weights.get(idx_in).copied().unwrap_or(1_000_000_000_000_000_000u128);
                let w_out = bal.weights.get(idx_out).copied().unwrap_or(1_000_000_000_000_000_000u128);
                balancer_output_amount(amount_in, balance_in, balance_out, w_in, w_out, bal.info.fee)
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use crate::pool::state::{PoolInfo, UniswapV2PoolState};
    use crate::pool::dex_type::DexType;

    fn usdc() -> Address { address!("2791bca1f2de4661ed88a30c99a7a9449aa84174") }
    fn wmatic() -> Address { address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270") }
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

    fn default_gas() -> GasConfig { GasConfig::default() }

    fn new_detector() -> MultiHopArbDetector { MultiHopArbDetector::new(1) }

    #[test]
    fn test_detect_empty_no_paths() {
        let pm = PoolManager::new();
        let mut detector = new_detector();
        assert!(detector.detect(&pm, 0, 100, 50_000_000_000, default_gas()).is_empty());
    }

    #[test]
    fn test_detect_three_pool_cyclic() {
        let mut pm = PoolManager::new();
        // Pool A: USDC/WMATIC — WMATIC cheap (0.5 USDC each)
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000));
        // Pool B: WMATIC/USDT — WMATIC expensive (2 USDT each)
        pm.add_pool(v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 500_000, 1_000_000));
        // Pool C: USDT/USDC — 1:1 rate (arb profit converts back to input token)
        pm.add_pool(v2_pool(address!("3333333333333333333333333333333333333333"), usdt(), usdc(), 1_000_000, 1_000_000));
        let mut detector = new_detector();
        let opps = detector.detect(&pm, 0, 100, 50_000_000_000, default_gas());
        assert!(!opps.is_empty());
        for opp in &opps {
            assert_eq!(opp.strategy, Strategy::MultiHopArb);
            assert!(opp.path.is_some());
            assert_eq!(opp.path.as_ref().unwrap().len(), 3);
        }
    }

    #[test]
    fn test_find_paths_three_pool_triangular() {
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000));
        pm.add_pool(v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 1_000_000, 500_000));
        pm.add_pool(v2_pool(address!("3333333333333333333333333333333333333333"), usdc(), usdt(), 1_000_000, 1_000_000));

        let paths = MultiHopArbDetector::find_paths(&pm, 4);
        assert!(paths.len() >= 2);
        let has_three_hop = paths.iter().any(|p| p.len() == 3);
        assert!(has_three_hop, "Should find at least one 3-pool path");
    }

    #[test]
    fn test_detect_three_pool_triangular() {
        let mut pm = PoolManager::new();
        // Pool A: USDC/WMATIC, cheap WMATIC (0.5 USDC)
        // Pool B: WMATIC/USDT, expensive WMATIC (2 USDT)
        // Pool C: USDC/USDT, 1:1
        // Arb: USDC -> A -> WMATIC -> B -> USDT -> C -> USDC
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000));
        pm.add_pool(v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 500_000, 1_000_000));
        pm.add_pool(v2_pool(address!("3333333333333333333333333333333333333333"), usdc(), usdt(), 1_000_000, 1_000_000));

        let mut detector = new_detector();
        let opps = detector.detect(&pm, 0, 100, 50_000_000_000, default_gas());
        assert!(!opps.is_empty(), "Should detect triangular arb");

        let paths_3: Vec<_> = opps.iter().filter(|o| o.path.as_ref().map(|p| p.len() >= 3).unwrap_or(false)).collect();
        assert!(!paths_3.is_empty(), "Should have at least one 3-pool path");

        for opp in paths_3 {
            assert!(opp.expected_profit > U256::ZERO);
            assert!(opp.gas_cost_wei > 0);
        }
    }

    #[test]
    fn test_no_cycle_repeat_pools() {
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 2_000_000));
        pm.add_pool(v2_pool(address!("2222222222222222222222222222222222222222"), usdc(), usdt(), 1_000_000, 1_000_000));

        let paths = MultiHopArbDetector::find_paths(&pm, 4);
        for path in &paths {
            let mut seen = std::collections::HashSet::new();
            for &addr in path {
                assert!(seen.insert(addr), "Duplicate pool {} in path {:?}", addr, path);
            }
        }
    }

    #[test]
    fn test_detect_no_profit_flat_prices() {
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(address!("1111111111111111111111111111111111111111"), usdc(), wmatic(), 1_000_000, 1_000_000));
        pm.add_pool(v2_pool(address!("2222222222222222222222222222222222222222"), wmatic(), usdt(), 1_000_000, 1_000_000));
        pm.add_pool(v2_pool(address!("3333333333333333333333333333333333333333"), usdc(), usdt(), 1_000_000, 1_000_000));

        let mut detector = new_detector();
        let opps = detector.detect(&pm, 0, 100, 50_000_000_000, default_gas());
        assert!(opps.is_empty());
    }
}
