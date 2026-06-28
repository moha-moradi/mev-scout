use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use super::extraction::{BlockCompetition, ExtractionType};

/// PGA parameters derived from observed competition data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgaCalibration {
    /// Per-strategy mean competitors observed.
    pub mean_competitors: HashMap<String, f64>,
    /// Per-strategy bid-to-value ratio (intensity proxy).
    pub bid_to_value_ratio: HashMap<String, f64>,
    /// Total blocks analyzed.
    pub blocks_analyzed: u64,
    /// Total extractions observed.
    pub total_extractions: u64,
}

/// Compute PGA calibration parameters from block competition data.
pub fn calibrate(blocks: &[BlockCompetition]) -> PgaCalibration {
    let mut blocks_analyzed = 0u64;
    let mut total_extractions = 0u64;

    let mut strategy_counts: HashMap<String, Vec<f64>> = HashMap::new();
    let mut strategy_bid_ratios: HashMap<String, Vec<f64>> = HashMap::new();

    for block in blocks {
        blocks_analyzed += 1;
        total_extractions += block.extractions.len() as u64;

        let mut seen_in_block: HashMap<String, bool> = HashMap::new();
        for ext in &block.extractions {
            let label = extraction_type_label(ext.extraction_type);
            seen_in_block.insert(label.to_string(), true);

            // bid-to-value ratio: priority_fee / (gross_profit / gas_used)
            if ext.gross_profit_wei > 0 && ext.gas_used > 0 {
                let profit_per_gas = ext.gross_profit_wei as f64 / ext.gas_used as f64;
                if profit_per_gas > 0.0 {
                    let ratio = ext.priority_fee_wei as f64 / profit_per_gas;
                    strategy_bid_ratios.entry(label.to_string()).or_default().push(ratio);
                }
            }
        }

        for (label, _) in seen_in_block {
            strategy_counts.entry(label).or_default().push(1.0f64);
        }
        let all_count = strategy_counts.entry("all".to_string()).or_default();
        all_count.push(block.unique_searchers as f64);
    }

    let mut mean_competitors = HashMap::new();
    for (strategy, counts) in &strategy_counts {
        if !counts.is_empty() {
            let sum: f64 = counts.iter().sum();
            mean_competitors.insert(strategy.clone(), sum / counts.len() as f64);
        }
    }

    let mut bid_to_value_ratio = HashMap::new();
    for (strategy, ratios) in &strategy_bid_ratios {
        if !ratios.is_empty() {
            let mut sorted = ratios.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = sorted[sorted.len() / 2];
            bid_to_value_ratio.insert(strategy.clone(), median);
        }
    }

    PgaCalibration {
        mean_competitors,
        bid_to_value_ratio,
        blocks_analyzed,
        total_extractions,
    }
}

fn extraction_type_label(et: ExtractionType) -> &'static str {
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
