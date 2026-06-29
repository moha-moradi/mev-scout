use std::collections::HashSet;
use alloy::primitives::{b256, Address, B256};
use serde::{Deserialize, Serialize};
use crate::data::ExecutedLog;
use crate::pool::decoders;
use crate::pool::state::PoolManager;
use crate::types::MevOpportunity;
use crate::types::Strategy;
use crate::utils::u128_from_be_bytes;

/// V2 Swap event topic: Swap(address,uint256,uint256,uint256,uint256,address)
const V2_SWAP_TOPIC: B256 = b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

/// Aave V3 LiquidationCall event signature.
static LIQUIDATION_CALL_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| alloy::primitives::keccak256("LiquidationCall(address,address,address,uint256,uint256,address,bool)"));

/// Identified MEV extraction type from on-chain tx analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExtractionType {
    TwoHopArb,
    MultiHopArb,
    Jit,
    JitArb,
    Sandwich,
    Liquidation,
    UnknownMev,
}

/// A single on-chain MEV extraction event attributed to a searcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitorExtraction {
    pub searcher: Address,
    pub extraction_type: ExtractionType,
    pub block_number: u64,
    pub tx_index: usize,
    pub gas_used: u64,
    pub gas_effective_wei: u128,
    pub priority_fee_wei: u128,
    pub gas_cost_wei: u128,
    pub gross_profit_wei: u128,
    pub net_profit_wei: i128,
    pub pools_involved: Vec<Address>,
    pub tokens_involved: Vec<Address>,
    pub builder: Address,
    pub matched_opportunity_id: Option<String>,
    pub confidence: f64,
}

/// Per-block competition snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockCompetition {
    pub block_number: u64,
    pub total_tx_count: usize,
    pub extractions: Vec<CompetitorExtraction>,
    pub unique_searchers: usize,
}

/// Tracks swap events within a single transaction for pattern analysis.
#[derive(Debug, Default)]
struct TxSwapAnalysis {
    pool_addresses: Vec<Address>,
    has_v3_mint: bool,
    has_v3_burn: bool,
    mint_sender: Option<Address>,
    burn_sender: Option<Address>,
    has_liquidation: bool,
    pool_indices: std::collections::HashMap<Address, Vec<(usize, usize)>>,
}

/// Compute the gross profit of an arbitrage transaction from its swap events.
/// Uses V2 amountIn/amountOut and V3 signed amount0/amount1 to compute net token flows.
fn compute_arb_profit(
    logs: &[ExecutedLog],
    pool_manager: &PoolManager,
    _sender: Address,
) -> (u128, Vec<Address>, Vec<Address>) {
    let mut pools_involved = Vec::new();
    let mut tokens_involved = Vec::new();
    let mut token_flows: std::collections::HashMap<Address, i128> = std::collections::HashMap::new();

    for log in logs {
        if log.topics.is_empty() {
            continue;
        }
        let t0 = log.topics[0];

        if t0 == V2_SWAP_TOPIC {
            let pool_addr = log.address;
            if pool_manager.get(&pool_addr).is_none() {
                continue;
            }
            if log.data.len() < 128 {
                continue;
            }
            let amt0_in = u128_from_be_bytes(&log.data[..32]) as i128;
            let amt1_in = u128_from_be_bytes(&log.data[32..64]) as i128;
            let amt0_out = u128_from_be_bytes(&log.data[64..96]) as i128;
            let amt1_out = u128_from_be_bytes(&log.data[96..128]) as i128;

            if let Some(pool) = pool_manager.get(&pool_addr) {
                let info = pool.info();
                pools_involved.push(pool_addr);
                if !tokens_involved.contains(&info.token0) {
                    tokens_involved.push(info.token0);
                }
                if !tokens_involved.contains(&info.token1) {
                    tokens_involved.push(info.token1);
                }
                *token_flows.entry(info.token0).or_insert(0) += amt0_out - amt0_in;
                *token_flows.entry(info.token1).or_insert(0) += amt1_out - amt1_in;
            }
        } else if t0 == decoders::V3_SWAP_TOPIC {
            let pool_addr = log.address;
            if pool_manager.get(&pool_addr).is_none() {
                continue;
            }
            if log.data.len() < 160 {
                continue;
            }
            let amount0_bytes: [u8; 32] = match log.data[..32].try_into() {
                Ok(b) => b,
                Err(_) => continue,
            };
            let amount0 = i128::from_be_bytes([
                amount0_bytes[16], amount0_bytes[17], amount0_bytes[18], amount0_bytes[19],
                amount0_bytes[20], amount0_bytes[21], amount0_bytes[22], amount0_bytes[23],
                amount0_bytes[24], amount0_bytes[25], amount0_bytes[26], amount0_bytes[27],
                amount0_bytes[28], amount0_bytes[29], amount0_bytes[30], amount0_bytes[31],
            ]);
            let amount1_bytes: [u8; 32] = match log.data[32..64].try_into() {
                Ok(b) => b,
                Err(_) => continue,
            };
            let amount1 = i128::from_be_bytes([
                amount1_bytes[16], amount1_bytes[17], amount1_bytes[18], amount1_bytes[19],
                amount1_bytes[20], amount1_bytes[21], amount1_bytes[22], amount1_bytes[23],
                amount1_bytes[24], amount1_bytes[25], amount1_bytes[26], amount1_bytes[27],
                amount1_bytes[28], amount1_bytes[29], amount1_bytes[30], amount1_bytes[31],
            ]);

            if let Some(pool) = pool_manager.get(&pool_addr) {
                let info = pool.info();
                pools_involved.push(pool_addr);
                if !tokens_involved.contains(&info.token0) {
                    tokens_involved.push(info.token0);
                }
                if !tokens_involved.contains(&info.token1) {
                    tokens_involved.push(info.token1);
                }
                *token_flows.entry(info.token0).or_insert(0) -= amount0;
                *token_flows.entry(info.token1).or_insert(0) -= amount1;
            }
        }
    }

    let gross = token_flows.values().filter(|&&v| v > 0).sum::<i128>() as u128;
    (gross, pools_involved, tokens_involved)
}

/// Classify a single transaction's extraction type from its event logs.
fn classify_extraction(
    logs: &[ExecutedLog],
    sender: Address,
    pool_manager: &PoolManager,
) -> (ExtractionType, f64) {
    let analysis = analyze_tx_logs(logs, sender, pool_manager);

    if analysis.has_liquidation {
        return (ExtractionType::Liquidation, 0.95);
    }

    let unique_pools: HashSet<&Address> = analysis.pool_addresses.iter().collect();
    let swap_pool_count = unique_pools.len();

    if analysis.has_v3_mint && analysis.has_v3_burn && swap_pool_count > 0 {
        if let (Some(m_sender), Some(b_sender)) = (analysis.mint_sender, analysis.burn_sender) {
            if m_sender == sender && b_sender == sender {
                if swap_pool_count >= 2 {
                    return (ExtractionType::JitArb, 0.85);
                }
                return (ExtractionType::Jit, 0.80);
            }
        }
    }

    if swap_pool_count >= 2 {
        match swap_pool_count {
            2 => (ExtractionType::TwoHopArb, 0.90),
            _ => (ExtractionType::MultiHopArb, 0.85),
        }
    } else if swap_pool_count == 1 && !analysis.has_liquidation {
        let pool_addr = analysis.pool_addresses[0];
        if pool_manager.get(&pool_addr).is_some() {
            if analysis.has_v3_mint || analysis.has_v3_burn {
                (ExtractionType::UnknownMev, 0.50)
            } else {
                (ExtractionType::UnknownMev, 0.30)
            }
        } else {
            (ExtractionType::UnknownMev, 0.20)
        }
    } else {
        (ExtractionType::UnknownMev, 0.10)
    }
}

/// Analyze event logs for a single transaction.
fn analyze_tx_logs(
    logs: &[ExecutedLog],
    _sender: Address,
    pool_manager: &PoolManager,
) -> TxSwapAnalysis {
    let mut analysis = TxSwapAnalysis::default();

    for log in logs {
        if log.topics.is_empty() {
            continue;
        }
        let t0 = log.topics[0];

        if t0 == V2_SWAP_TOPIC {
            if pool_manager.get(&log.address).is_some() {
                let idx = analysis.pool_addresses.len();
                analysis.pool_addresses.push(log.address);
                analysis.pool_indices.entry(log.address).or_default().push((0, idx));
            }
        } else if t0 == decoders::V3_SWAP_TOPIC {
            if pool_manager.get(&log.address).is_some() {
                let idx = analysis.pool_addresses.len();
                analysis.pool_addresses.push(log.address);
                analysis.pool_indices.entry(log.address).or_default().push((1, idx));
            }
        } else if t0 == *decoders::V3_MINT_TOPIC {
            analysis.has_v3_mint = true;
            if log.topics.len() > 1 {
                let addr_bytes = &log.topics[1].as_slice()[12..];
                if addr_bytes.len() == 20 {
                    analysis.mint_sender = Some(Address::from_slice(addr_bytes));
                }
            }
        } else if t0 == decoders::V3_BURN_TOPIC {
            analysis.has_v3_burn = true;
            if log.topics.len() > 1 {
                let addr_bytes = &log.topics[1].as_slice()[12..];
                if addr_bytes.len() == 20 {
                    analysis.burn_sender = Some(Address::from_slice(addr_bytes));
                }
            }
        } else if t0 == *decoders::CURVE_TOKEN_EXCHANGE_TOPIC
            || t0 == *decoders::CURVE_V2_TOKEN_EXCHANGE_TOPIC
        {
            if pool_manager.get(&log.address).is_some() {
                analysis.pool_addresses.push(log.address);
            }
        } else if t0 == *decoders::BALANCER_SWAP_TOPIC {
            if pool_manager.get(&log.address).is_some() {
                analysis.pool_addresses.push(log.address);
            }
        } else if t0 == *LIQUIDATION_CALL_TOPIC {
            analysis.has_liquidation = true;
        }
    }

    analysis
}

/// Cross-reference extractions with detected opportunities.
fn match_opportunity(
    extraction: &CompetitorExtraction,
    opportunities: &[MevOpportunity],
) -> Option<String> {
    for opp in opportunities {
        if opp.block_number != extraction.block_number {
            continue;
        }
        match extraction.extraction_type {
            ExtractionType::TwoHopArb | ExtractionType::MultiHopArb => {
                if extraction.pools_involved.contains(&opp.pool_a)
                    && (opp.pool_b == Address::ZERO || extraction.pools_involved.contains(&opp.pool_b))
                {
                    return opp.canonical_id.clone();
                }
            }
            ExtractionType::Sandwich => {
                if let Some(victim) = opp.victim_tx_index {
                    if victim == extraction.tx_index
                        || victim.wrapping_sub(1) == extraction.tx_index
                        || victim.wrapping_add(1) == extraction.tx_index
                    {
                        return opp.canonical_id.clone();
                    }
                }
            }
            ExtractionType::Liquidation => {
                if extraction.pools_involved.contains(&opp.pool_a) {
                    return opp.canonical_id.clone();
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract competitor information from a single block's replayed transactions.
/// Called after all transactions in a block have been processed.
/// Build a set of sandwich-related tx indices from opportunities.
/// Returns just the tx indices (sender matching is verified separately).
fn sandwich_tx_index_set(opportunities: &[MevOpportunity]) -> HashSet<usize> {
    opportunities
        .iter()
        .filter(|opp| opp.strategy == Strategy::Sandwich)
        .flat_map(|opp| {
            let mut indices = Vec::new();
            if let Some(victim) = opp.victim_tx_index {
                if victim > 0 {
                    indices.push(victim - 1); // frontrun
                }
            }
            if let Some(backrun) = opp.backrun_tx_index {
                indices.push(backrun);
            }
            indices
        })
        .collect()
}

/// Extract competitor information from a single block's replayed transactions.
/// Called after all transactions in a block have been processed.
pub fn analyze_block(
    block_number: u64,
    txs: &[(usize, Address, u64, u128, Vec<ExecutedLog>)],
    pool_manager: &PoolManager,
    opportunities: &[MevOpportunity],
    base_fee_per_gas: u128,
    builder: Address,
) -> BlockCompetition {
    // Pre-compute sandwich-related tx indices from detected opportunities
    let sandwich_indices = sandwich_tx_index_set(opportunities);

    let mut extractions = Vec::new();
    let mut searchers_seen = HashSet::new();

    for (tx_index, sender, gas_used, gas_effective, logs) in txs {
        let (extraction_type, confidence) = if sandwich_indices.contains(tx_index) {
            (ExtractionType::Sandwich, 0.90)
        } else {
            classify_extraction(logs, *sender, pool_manager)
        };

        if confidence < 0.25 {
            continue;
        }

        let priority_fee_wei = gas_effective.saturating_sub(base_fee_per_gas);
        let gas_cost_wei = (*gas_used as u128).saturating_mul(*gas_effective);
        let (gross_profit_wei, pools_involved, tokens_involved) =
            compute_arb_profit(logs, pool_manager, *sender);

        if gross_profit_wei == 0 && extraction_type == ExtractionType::UnknownMev {
            continue;
        }

        let net_profit_wei = (gross_profit_wei as i128).saturating_sub(gas_cost_wei as i128);
        if net_profit_wei <= 0 && extraction_type != ExtractionType::Liquidation && extraction_type != ExtractionType::Sandwich {
            continue;
        }

        let extraction = CompetitorExtraction {
            searcher: *sender,
            extraction_type,
            block_number,
            tx_index: *tx_index,
            gas_used: *gas_used,
            gas_effective_wei: *gas_effective,
            priority_fee_wei,
            gas_cost_wei,
            gross_profit_wei,
            net_profit_wei,
            pools_involved,
            tokens_involved,
            builder,
            matched_opportunity_id: None,
            confidence,
        };

        searchers_seen.insert(*sender);
        extractions.push(extraction);
    }

    for extraction in &mut extractions {
        extraction.matched_opportunity_id = match_opportunity(extraction, opportunities);
    }

    BlockCompetition {
        block_number,
        total_tx_count: txs.len(),
        extractions,
        unique_searchers: searchers_seen.len(),
    }
}
