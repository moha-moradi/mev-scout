use alloy::primitives::{Address, U256};
use serde::{Deserialize, Serialize};

use crate::mev::opportunity::MevOpportunity;
use crate::mev::two_hop::{balancer_output_amount, curve_output_amount};
use crate::pool::math::constant_product_output_amount;
use crate::pool::state::{PoolManager, PoolState};
use crate::pool::v3_quote::quote_v3_exact_in;
use crate::types::Strategy;

/// Per-block stats collected during a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockReplayStats {
    pub block_number: u64,
    pub total_tx_count: usize,
    pub dex_tx_count: usize,
}

/// Per-block summary from a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSummary {
    pub block_number: u64,
    pub total_tx: usize,
    pub dex_tx: usize,
    pub opportunities: usize,
    pub by_strategy: std::collections::HashMap<String, usize>,
}

/// Fact-check result for a single opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityFactCheck {
    pub block_number: u64,
    pub tx_index: usize,
    pub strategy: String,
    pub pool_a: Address,
    pub pool_b: Address,
    pub pool_a_name: Option<String>,
    pub pool_b_name: Option<String>,
    pub token_in: Address,
    pub token_out: Address,
    pub input_amount: String,
    pub expected_profit: String,
    pub gas_cost_wei: u128,
    pub profit_gt_gas: bool,
    pub recomputed_profit: Option<String>,
    pub recomputation_match: Option<bool>,
    pub victim_tx_index: Option<usize>,
    pub backrun_tx_index: Option<usize>,
    pub tick_lower: Option<i32>,
    pub tick_upper: Option<i32>,
    pub liquidity_amount: Option<u128>,
    pub path: Option<Vec<Address>>,
}

/// Full fact-check report for a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactCheckReport {
    pub run_id: String,
    pub chain: String,
    pub block_count: usize,
    pub total_opportunities: usize,
    pub passed: usize,
    pub failed: usize,
    pub block_summaries: Vec<BlockSummary>,
    pub opportunity_checks: Vec<OpportunityFactCheck>,
}

/// Format a U256 token amount as a human-readable decimal string.
///
/// `decimals` is the number of decimal places for the token (e.g. 18 for WETH, 6 for USDC).
/// Default to 18 when the actual token decimals are unknown.
fn format_amount(val: &alloy::primitives::U256, decimals: u8) -> String {
    let s = val.to_string();
    let d = decimals as usize;
    if s.len() > d {
        let (int_part, dec_part) = s.split_at(s.len() - d);
        let dec_trimmed = dec_part.trim_end_matches('0');
        if dec_trimmed.is_empty() {
            format!("{}.0", int_part)
        } else {
            format!("{}.{}", int_part, dec_trimmed)
        }
    } else {
        format!("0.{:0>width$}", s, width = d).trim_end_matches('0').to_string()
    }
}

/// Compute per-block summaries from opportunities and per-block tx/dex counts.
pub fn compute_block_summaries(
    opportunities: &[MevOpportunity],
    per_block_stats: &[BlockReplayStats],
) -> Vec<BlockSummary> {
    let stats_map: std::collections::HashMap<u64, &BlockReplayStats> =
        per_block_stats.iter().map(|s| (s.block_number, s)).collect();

    let mut opps_by_block: std::collections::HashMap<u64, Vec<&MevOpportunity>> =
        std::collections::HashMap::new();
    for opp in opportunities {
        opps_by_block.entry(opp.block_number).or_default().push(opp);
    }

    let mut summaries = Vec::new();
    let mut block_numbers: Vec<u64> = stats_map.keys().copied().collect();
    block_numbers.sort();

    for block_num in block_numbers {
        let stats = stats_map[&block_num];
        let opps = opps_by_block.remove(&block_num).unwrap_or_default();
        let mut by_strategy = std::collections::HashMap::new();
        for opp in &opps {
            *by_strategy.entry(opp.strategy.to_string()).or_insert(0) += 1;
        }
        summaries.push(BlockSummary {
            block_number: block_num,
            total_tx: stats.total_tx_count,
            dex_tx: stats.dex_tx_count,
            opportunities: opps.len(),
            by_strategy,
        });
    }

    summaries
}

/// Quote a single swap through any pool type.
/// Returns the output amount for the given `amount_in` of `token_in`.
pub fn quote_single_swap(
    pool: &PoolState,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
) -> Option<u128> {
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
            let idx_out = *curve.token_index.get(&token_out)?;
            let balance_in = curve.balances[idx_in];
            let balance_out = curve.balances[idx_out];
            if balance_in == 0 || balance_out == 0 {
                return None;
            }
            curve_output_amount(amount_in, balance_in, balance_out, curve.info.fee, curve.a_coeff)
        }
        PoolState::Balancer(bal) => {
            let idx_in = *bal.token_index.get(&token_in)?;
            let idx_out = *bal.token_index.get(&token_out)?;
            let balance_in = bal.balances[idx_in];
            let balance_out = bal.balances[idx_out];
            if balance_in == 0 || balance_out == 0 {
                return None;
            }
            let w_in = bal.weights.get(idx_in).copied().unwrap_or(1_000_000_000_000_000_000u128);
            let w_out = bal.weights.get(idx_out).copied().unwrap_or(1_000_000_000_000_000_000u128);
            balancer_output_amount(amount_in, balance_in, balance_out, w_in, w_out, bal.info.fee)
        }
    }
}

/// Recompute the gross profit for a detected MEV opportunity using the
/// current pool manager state.
///
/// Returns `None` when the strategy is not supported for recomputation or
/// when the necessary pools are no longer tracked.
/// The returned profit is the raw (gross) profit in `token_out` before
/// flash loan fee deduction and normalization.
pub fn recompute_opportunity_profit(
    pools: &PoolManager,
    opp: &MevOpportunity,
) -> Option<U256> {
    let input = opp.input_amount.to::<u128>();
    if input == 0 {
        return None;
    }

    match opp.strategy {
        Strategy::TwoHopArb => {
            let pool_a = pools.get(&opp.pool_a)?;
            let pool_b = pools.get(&opp.pool_b)?;

            let info_a = pool_a.info();
            let info_b = pool_b.info();

            let shared = if info_a.token0 == info_b.token0 || info_a.token0 == info_b.token1 {
                info_a.token0
            } else {
                info_a.token1
            };

            let intermediate = quote_single_swap(pool_a, opp.token_in, shared, input)?;
            let output = quote_single_swap(pool_b, shared, opp.token_out, intermediate)?;

            if output <= input {
                return None;
            }
            Some(U256::from(output - input))
        }
        Strategy::MultiHopArb => {
            let path = opp.path.as_ref()?;
            if path.is_empty() {
                return None;
            }
            let mut current = input;
            let mut current_token = opp.token_in;
            for &addr in path {
                let pool = pools.get(&addr)?;
                let info = pool.info();
                let next_token = if info.token0 == current_token {
                    info.token1
                } else if info.token1 == current_token {
                    info.token0
                } else {
                    return None;
                };
                current = quote_single_swap(pool, current_token, next_token, current)?;
                current_token = next_token;
            }
            if current <= input {
                return None;
            }
            Some(U256::from(current - input))
        }
        _ => None,
    }
}

/// Build opportunity fact checks from saved results.
///
/// If `pools` is `Some`, recomputes each opportunity's profit using the
/// current pool state and fills in `recomputed_profit` and `recomputation_match`.
pub fn verify_opportunities(
    opportunities: &[MevOpportunity],
    pools: Option<&PoolManager>,
) -> Vec<OpportunityFactCheck> {
    opportunities
        .iter()
        .map(|opp| {
            let profit_gt_gas = opp.expected_profit > U256::from(opp.gas_cost_wei);
            let (recomputed_profit, recomputation_match) = pools
                .and_then(|pm| recompute_opportunity_profit(pm, opp))
                .map(|recomputed| {
                    let stored = opp.raw_profit.unwrap_or(opp.expected_profit);
                    let matched = stored == recomputed;
                    (Some(format_amount(&recomputed, 18)), Some(matched))
                })
                .unwrap_or((None, None));
            OpportunityFactCheck {
                block_number: opp.block_number,
                tx_index: opp.tx_index,
                strategy: opp.strategy.to_string(),
                pool_a: opp.pool_a,
                pool_b: opp.pool_b,
                pool_a_name: None,
                pool_b_name: None,
                token_in: opp.token_in,
                token_out: opp.token_out,
                input_amount: format_amount(&opp.input_amount, 18),
                expected_profit: format_amount(&opp.expected_profit, 18),
                gas_cost_wei: opp.gas_cost_wei,
                profit_gt_gas,
                recomputed_profit,
                recomputation_match,
                victim_tx_index: opp.victim_tx_index,
                backrun_tx_index: opp.backrun_tx_index,
                tick_lower: opp.tick_lower,
                tick_upper: opp.tick_upper,
                liquidity_amount: opp.liquidity_amount,
                path: opp.path.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Strategy;
    use alloy::primitives::{address, U256};

    #[test]
    fn test_compute_block_summaries_empty() {
        let result = compute_block_summaries(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_block_summaries_single_block() {
        let stats = vec![BlockReplayStats {
            block_number: 1,
            total_tx_count: 100,
            dex_tx_count: 25,
        }];
        let opps = vec![
            MevOpportunity {
                block_number: 1,
                tx_index: 5,
                strategy: Strategy::TwoHopArb,
                ..MevOpportunity::new(1, 5, Strategy::TwoHopArb, address!("1111111111111111111111111111111111111111"), 100)
            },
            MevOpportunity {
                block_number: 1,
                tx_index: 10,
                strategy: Strategy::Sandwich,
                ..MevOpportunity::new(1, 10, Strategy::Sandwich, address!("2222222222222222222222222222222222222222"), 100)
            },
        ];

        let summaries = compute_block_summaries(&opps, &stats);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].block_number, 1);
        assert_eq!(summaries[0].total_tx, 100);
        assert_eq!(summaries[0].dex_tx, 25);
        assert_eq!(summaries[0].opportunities, 2);
        assert_eq!(summaries[0].by_strategy.get("two_hop_arb"), Some(&1));
        assert_eq!(summaries[0].by_strategy.get("sandwich"), Some(&1));
    }

    #[test]
    fn test_compute_block_summaries_multiple_blocks() {
        let stats = vec![
            BlockReplayStats {
                block_number: 1,
                total_tx_count: 100,
                dex_tx_count: 25,
            },
            BlockReplayStats {
                block_number: 2,
                total_tx_count: 50,
                dex_tx_count: 10,
            },
        ];
        let opps = vec![
            MevOpportunity::new(1, 5, Strategy::TwoHopArb, address!("1111111111111111111111111111111111111111"), 100),
            MevOpportunity::new(1, 10, Strategy::Sandwich, address!("2222222222222222222222222222222222222222"), 100),
        ];

        let summaries = compute_block_summaries(&opps, &stats);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].block_number, 1);
        assert_eq!(summaries[0].opportunities, 2);
        assert_eq!(summaries[1].block_number, 2);
        assert_eq!(summaries[1].opportunities, 0);
    }

    #[test]
    fn test_verify_opportunities_sandwich() {
        let opp = MevOpportunity {
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::Sandwich,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: Address::ZERO,
            token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_out: address!("cccccccccccccccccccccccccccccccccccccccc"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(500),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            gas_cost_wei: 100,
            timestamp: 12345,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: Some(1),
            backrun_tx_index: Some(2),
        };
        let checks = verify_opportunities(&[opp], None);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].profit_gt_gas);
        assert_eq!(checks[0].victim_tx_index, Some(1));
        assert_eq!(checks[0].backrun_tx_index, Some(2));
    }

    #[test]
    fn test_verify_opportunities_missing_sandwich_fields() {
        let opp = MevOpportunity::new(1, 0, Strategy::Sandwich, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100);
        let checks = verify_opportunities(&[opp], None);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].victim_tx_index.is_none());
        assert!(checks[0].backrun_tx_index.is_none());
    }

    #[test]
    fn test_verify_opportunities_profit_vs_gas() {
        let profitable = MevOpportunity {
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::TwoHopArb,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_in: address!("cccccccccccccccccccccccccccccccccccccccc"),
            token_out: address!("dddddddddddddddddddddddddddddddddddddddd"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(500),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            gas_cost_wei: 100,
            timestamp: 12345,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
        };
        let unprofitable = MevOpportunity {
            expected_profit: U256::from(50),
            ..profitable.clone()
        };

        let checks = verify_opportunities(&[profitable, unprofitable], None);
        assert_eq!(checks.len(), 2);
        assert!(checks[0].profit_gt_gas);
        assert!(!checks[1].profit_gt_gas);
    }

    #[test]
    fn test_format_amount() {
        // 1 ether = 10^18 wei
        let one_eth = U256::from(10u64).pow(U256::from(18));
        assert!(format_amount(&one_eth, 18).contains("1.0"));

        let zero = U256::ZERO;
        assert!(format_amount(&zero, 18).contains("0"));
    }
}
