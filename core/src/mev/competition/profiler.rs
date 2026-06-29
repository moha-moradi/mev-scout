use std::collections::HashMap;
use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use crate::types::Strategy;
use super::extraction::{BlockCompetition, ExtractionType};

/// Compiled profile of a single competitor searcher across blocks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompetitorProfile {
    pub searcher: Address,
    pub total_extractions: u64,
    pub by_strategy: HashMap<ExtractionType, u64>,
    pub total_gas_spent_wei: u128,
    pub total_gross_profit_wei: u128,
    pub total_net_profit_wei: i128,
    pub avg_priority_fee_wei: u128,
    pub avg_gas_price_wei: u128,
    pub preferred_builders: Vec<Address>,
    pub first_seen_block: u64,
    pub last_seen_block: u64,
}

/// Build competitor profiles from a sequence of block competition data.
pub fn build_profiles(blocks: &[BlockCompetition]) -> Vec<CompetitorProfile> {
    let mut searcher_data: HashMap<Address, ProfileAccumulator> = HashMap::new();

    for block in blocks {
        for ext in &block.extractions {
            let acc = searcher_data.entry(ext.searcher).or_insert_with(|| ProfileAccumulator::new(ext.searcher));
            acc.total_extractions += 1;
            *acc.by_strategy.entry(ext.extraction_type).or_insert(0) += 1;
            acc.total_gas_spent_wei = acc.total_gas_spent_wei.saturating_add(ext.gas_cost_wei);
            acc.total_gross_profit_wei = acc.total_gross_profit_wei.saturating_add(ext.gross_profit_wei);
            acc.total_net_profit_wei = acc.total_net_profit_wei.saturating_add(ext.net_profit_wei);
            acc.priority_fee_sum += ext.priority_fee_wei;
            acc.gas_price_sum += ext.gas_effective_wei;
            acc.extraction_count += 1;
            acc.first_seen = acc.first_seen.min(ext.block_number);
            acc.last_seen = acc.last_seen.max(ext.block_number);
            if !acc.builders.contains(&ext.builder) {
                acc.builders.push(ext.builder);
            }
        }
    }

    let mut profiles: Vec<CompetitorProfile> = searcher_data.into_values().map(|acc| CompetitorProfile {
        searcher: acc.searcher,
        total_extractions: acc.total_extractions,
        by_strategy: acc.by_strategy,
        total_gas_spent_wei: acc.total_gas_spent_wei,
        total_gross_profit_wei: acc.total_gross_profit_wei,
        total_net_profit_wei: acc.total_net_profit_wei,
        avg_priority_fee_wei: if acc.extraction_count > 0 {
            acc.priority_fee_sum / acc.extraction_count as u128
        } else { 0 },
        avg_gas_price_wei: if acc.extraction_count > 0 {
            acc.gas_price_sum / acc.extraction_count as u128
        } else { 0 },
        preferred_builders: acc.builders,
        first_seen_block: acc.first_seen,
        last_seen_block: acc.last_seen,
    }).collect();

    profiles.sort_by(|a, b| b.total_extractions.cmp(&a.total_extractions));
    profiles
}

struct ProfileAccumulator {
    searcher: Address,
    total_extractions: u64,
    by_strategy: HashMap<ExtractionType, u64>,
    total_gas_spent_wei: u128,
    total_gross_profit_wei: u128,
    total_net_profit_wei: i128,
    priority_fee_sum: u128,
    gas_price_sum: u128,
    extraction_count: u64,
    builders: Vec<Address>,
    first_seen: u64,
    last_seen: u64,
}

impl ProfileAccumulator {
    fn new(searcher: Address) -> Self {
        ProfileAccumulator {
            searcher,
            total_extractions: 0,
            by_strategy: HashMap::new(),
            total_gas_spent_wei: 0,
            total_gross_profit_wei: 0,
            total_net_profit_wei: 0,
            priority_fee_sum: 0,
            gas_price_sum: 0,
            extraction_count: 0,
            builders: Vec::new(),
            first_seen: u64::MAX,
            last_seen: 0,
        }
    }
}

/// Convert ExtractionType to Strategy for cross-referencing.
pub fn extraction_type_to_strategy(et: ExtractionType) -> Option<Strategy> {
    match et {
        ExtractionType::TwoHopArb => Some(Strategy::TwoHopArb),
        ExtractionType::MultiHopArb => Some(Strategy::MultiHopArb),
        ExtractionType::Jit => Some(Strategy::Jit),
        ExtractionType::JitArb => Some(Strategy::JitArb),
        ExtractionType::Sandwich => Some(Strategy::Sandwich),
        ExtractionType::Liquidation => Some(Strategy::Liquidation),
        ExtractionType::UnknownMev => None,
    }
}

/// Strategy labels for display
pub fn extraction_type_label(et: ExtractionType) -> &'static str {
    match et {
        ExtractionType::TwoHopArb => "two_hop_arb",
        ExtractionType::MultiHopArb => "multi_hop_arb",
        ExtractionType::Jit => "jit",
        ExtractionType::JitArb => "jit_arb",
        ExtractionType::Sandwich => "sandwich",
        ExtractionType::Liquidation => "liquidation",
        ExtractionType::UnknownMev => "unknown",
    }
}
