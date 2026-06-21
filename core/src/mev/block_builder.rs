use crate::mev::opportunity::MevOpportunity;
use alloy::primitives::{Address, U256};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct BlockBuilderConfig {
    /// Block gas limit (default 30_000_000 for Ethereum mainnet)
    pub block_gas_limit: u128,
    /// Maximum ops per bundle
    pub max_ops_per_bundle: usize,
}

impl Default for BlockBuilderConfig {
    fn default() -> Self {
        Self {
            block_gas_limit: 30_000_000,
            max_ops_per_bundle: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BundleOp {
    pub opportunity: MevOpportunity,
    pub profit_per_gas: f64,
}

#[derive(Debug, Clone)]
pub struct BlockBundle {
    pub block_number: u64,
    pub ops: Vec<MevOpportunity>,
    pub total_profit: U256,
    pub total_gas: u128,
    pub op_count: usize,
}

fn profit_to_f64(val: &U256, gas: u128) -> f64 {
    // Profits fit in u128 in practice; convert to f64 for sorting ratio
    val.to::<u128>() as f64 / gas as f64
}

/// Build a single block from opportunities by:
/// 1. Sorting by profit/gas (descending)
/// 2. Greedy-packing within block gas limit
/// 3. Rejecting ops that share pools (conflict)
pub fn build_block(
    opportunities: Vec<MevOpportunity>,
    block_number: u64,
    config: &BlockBuilderConfig,
) -> BlockBundle {
    let mut candidates: Vec<BundleOp> = opportunities
        .into_iter()
        .filter(|opp| opp.gas_cost_wei > 0 && opp.expected_profit > U256::ZERO)
        .map(|opp| {
            let p_g = profit_to_f64(&opp.expected_profit, opp.gas_cost_wei);
            BundleOp { opportunity: opp, profit_per_gas: p_g }
        })
        .collect();

    candidates.sort_by(|a, b| b.profit_per_gas.partial_cmp(&a.profit_per_gas).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected: Vec<MevOpportunity> = Vec::new();
    let mut used_pools: HashSet<Address> = HashSet::new();
    let mut gas_used: u128 = 0;
    let mut total_profit = U256::ZERO;

    for bundle_op in &candidates {
        if selected.len() >= config.max_ops_per_bundle {
            break;
        }
        let gas = bundle_op.opportunity.gas_cost_wei;
        if gas_used.saturating_add(gas) > config.block_gas_limit {
            continue;
        }
        let pa = bundle_op.opportunity.pool_a;
        let pb = bundle_op.opportunity.pool_b;
        if used_pools.contains(&pa) || used_pools.contains(&pb) {
            continue;
        }
        used_pools.insert(pa);
        used_pools.insert(pb);
        gas_used = gas_used.saturating_add(gas);
        total_profit = total_profit.saturating_add(bundle_op.opportunity.expected_profit);
        selected.push(bundle_op.opportunity.clone());
    }

    BlockBundle {
        block_number,
        op_count: selected.len(),
        total_profit,
        total_gas: gas_used,
        ops: selected,
    }
}

/// Build bundles across multiple blocks, returning only selected opportunities.
pub fn build_bundles(
    opportunities: Vec<MevOpportunity>,
    config: &BlockBuilderConfig,
) -> Vec<MevOpportunity> {
    let mut by_block: std::collections::BTreeMap<u64, Vec<MevOpportunity>> =
        std::collections::BTreeMap::new();
    for opp in opportunities {
        by_block.entry(opp.block_number).or_default().push(opp);
    }

    let mut result = Vec::new();
    for (block_num, opps) in by_block {
        let bundle = build_block(opps, block_num, config);
        result.extend(bundle.ops);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Strategy;

    fn make_opp(block: u64, profit: u128, gas: u128, pool_a: Address, pool_b: Address) -> MevOpportunity {
        MevOpportunity {
            canonical_id: None,
            block_number: block,
            tx_index: 0,
            strategy: Strategy::TwoHopArb,
            pool_a,
            pool_b,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            input_amount: U256::ZERO,
            expected_profit: U256::from(profit),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            pga_adjusted_profit: None,
            gas_cost_wei: gas,
            timestamp: 0,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
        }
    }

    #[test]
    fn test_build_block_selects_most_profitable_per_gas() {
        let pool1 = Address::repeat_byte(0x01);
        let pool2 = Address::repeat_byte(0x02);
        let pool3 = Address::repeat_byte(0x03);
        let pool4 = Address::repeat_byte(0x04);

        // op1: 10 ETH profit, 1M gas → 10.0 profit/gas
        // op2: 5 ETH profit, 1M gas → 5.0 profit/gas
        // op3: 1 ETH profit, 100k gas → 10.0 profit/gas (same ratio, should also be included)
        let opps = vec![
            make_opp(1, 10_000_000_000_000_000_000, 1_000_000, pool1, pool2),
            make_opp(1, 5_000_000_000_000_000_000, 1_000_000, pool3, pool4),
        ];
        let config = BlockBuilderConfig::default();
        let bundle = build_block(opps, 1, &config);
        assert_eq!(bundle.op_count, 2, "both ops should fit within gas limit");
        assert_eq!(bundle.total_gas, 2_000_000);
    }

    #[test]
    fn test_build_block_rejects_conflicting_pools() {
        let pool1 = Address::repeat_byte(0x01);
        let pool2 = Address::repeat_byte(0x02);
        let pool3 = Address::repeat_byte(0x03);

        // op2 shares pool1 with op1 → should be rejected
        let opps = vec![
            make_opp(1, 10_000_000_000_000_000_000, 1_000_000, pool1, pool2),
            make_opp(1, 5_000_000_000_000_000_000, 500_000, pool1, pool3),
        ];
        let config = BlockBuilderConfig::default();
        let bundle = build_block(opps, 1, &config);
        assert_eq!(bundle.op_count, 1, "conflicting op should be rejected");
    }

    #[test]
    fn test_build_block_respects_gas_limit() {
        let pool1 = Address::repeat_byte(0x01);
        let pool2 = Address::repeat_byte(0x02);
        let pool3 = Address::repeat_byte(0x03);
        let pool4 = Address::repeat_byte(0x04);

        let opps = vec![
            make_opp(1, 10_000_000_000_000_000_000, 20_000_000, pool1, pool2),
            make_opp(1, 5_000_000_000_000_000_000, 20_000_000, pool3, pool4),
        ];
        let mut config = BlockBuilderConfig::default();
        config.block_gas_limit = 25_000_000;
        let bundle = build_block(opps, 1, &config);
        assert_eq!(bundle.op_count, 1, "second op should exceed remaining gas");
    }

    #[test]
    fn test_build_bundles_multi_block() {
        let pool1 = Address::repeat_byte(0x01);
        let pool2 = Address::repeat_byte(0x02);

        let opps = vec![
            make_opp(1, 10_000_000_000_000_000_000, 1_000_000, pool1, pool2),
            make_opp(2, 5_000_000_000_000_000_000, 1_000_000, pool1, pool2),
        ];
        let config = BlockBuilderConfig::default();
        let result = build_bundles(opps, &config);
        assert_eq!(result.len(), 2, "both blocks should have one op each");
    }
}
