use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use super::extraction::{BlockCompetition, ExtractionType};
use super::profiler::CompetitorProfile;
use super::calibrator::PgaCalibration;

/// Aggregated competition report for an entire backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitionReport {
    pub total_searchers_found: usize,
    pub total_extractions: usize,
    pub by_strategy: HashMap<String, usize>,
    pub per_block: Vec<BlockCompetition>,
    pub top_searchers: Vec<CompetitorProfile>,
    pub pga_calibration: PgaCalibration,
}

impl CompetitionReport {
    /// Build a report from per-block competition data with calibration.
    pub fn new(blocks: Vec<BlockCompetition>) -> Self {
        let total_searchers: std::collections::HashSet<_> = blocks
            .iter()
            .flat_map(|b| b.extractions.iter().map(|e| e.searcher))
            .collect();

        let total_extractions: usize = blocks.iter().map(|b| b.extractions.len()).sum();

        let mut by_strategy: HashMap<String, usize> = HashMap::new();
        for block in &blocks {
            for ext in &block.extractions {
                let label = match ext.extraction_type {
                    ExtractionType::TwoHopArb => "two_hop_arb",
                    ExtractionType::MultiHopArb => "multi_hop_arb",
                    ExtractionType::Jit => "jit",
                    ExtractionType::JitArb => "jit_arb",
                    ExtractionType::Sandwich => "sandwich",
                    ExtractionType::Liquidation => "liquidation",
                    ExtractionType::UnknownMev => "unknown",
                };
                *by_strategy.entry(label.to_string()).or_insert(0) += 1;
            }
        }

        let profiles = super::profiler::build_profiles(&blocks);
        let calibration = super::calibrator::calibrate(&blocks);

        CompetitionReport {
            total_searchers_found: total_searchers.len(),
            total_extractions,
            by_strategy,
            per_block: blocks,
            top_searchers: profiles,
            pga_calibration: calibration,
        }
    }

    /// Return the top N searchers by extraction count.
    pub fn top_searchers(&self, n: usize) -> &[CompetitorProfile] {
        let end = n.min(self.top_searchers.len());
        &self.top_searchers[..end]
    }
}
