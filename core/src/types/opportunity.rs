//! Data contracts for detected MEV opportunities and persisted result files.
//!
//! These types are the serialization boundary between the core backtest engine,
//! the CLI output layer, and the API serialization layer.

use alloy::primitives::{Address, U256};
use serde::{Deserialize, Serialize};
use crate::types::strategy::Strategy;
use crate::mev::competition::report::CompetitionReport;

/// A detected MEV opportunity from backtesting.
///
/// Different strategies populate different optional fields:
/// - `path` for multi-hop strategies,
/// - `tick_lower`/`tick_upper`/`liquidity_amount` for JIT strategies,
/// - `victim_tx_index`/`backrun_tx_index` for sandwich attacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevOpportunity {
    /// Canonical dedup ID (L9): derived from strategy + key fields to uniquely
    /// identify this opportunity across detectors and aggregation passes.
    /// Example: "TwoHopArb|0xaaa|0xbbb|0xccc|0xddd" or "Sandwich|0xaaa|tx:1|tx:2".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_id: Option<String>,
    /// Block where the opportunity was detected
    pub block_number: u64,
    /// Index of the transaction after which the opportunity exists
    pub tx_index: usize,
    /// The strategy type
    pub strategy: Strategy,
    /// Pool involved in the first swap
    pub pool_a: Address,
    /// Pool involved in the second swap
    pub pool_b: Address,
    /// Token being arbitraged (input token)
    pub token_in: Address,
    /// Token received as output
    pub token_out: Address,
    /// Amount of token_in to invest
    pub input_amount: U256,
    /// Expected profit in token_out (gross, before gas)
    pub expected_profit: U256,
    /// Raw profit in token_out before normalization to native (None = same as expected_profit)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_profit: Option<U256>,
    /// Profit estimate with +1% slippage (more input) — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_p1: Option<U256>,
    /// Profit estimate with -1% slippage (less input) — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_m1: Option<U256>,
    /// Profit estimate with +2% slippage — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_p2: Option<U256>,
    /// Profit estimate with -2% slippage — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_m2: Option<U256>,
    /// Profit after PGA simulation (competition-adjusted) — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pga_adjusted_profit: Option<U256>,
    /// Estimated gas cost in wei
    pub gas_cost_wei: u128,
    /// Timestamp of the block
    pub timestamp: u64,
    /// Full pool path for multi-hop opportunities (e.g., [buy, intermediate, ..., sell])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<Address>>,
    /// Tick range lower bound (JIT liquidity positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_lower: Option<i32>,
    /// Tick range upper bound (JIT liquidity positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_upper: Option<i32>,
    /// Amount of liquidity deployed (JIT positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liquidity_amount: Option<u128>,
    /// Transaction index of the victim's swap (sandwich attacks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victim_tx_index: Option<usize>,
    /// Transaction index of the backrun (sandwich attacks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backrun_tx_index: Option<usize>,
    /// Whether this opportunity was detected from mempool/pending transactions
    /// rather than settled on-chain blocks.
    #[serde(default)]
    pub mempool_only: bool,
    /// Confidence score (0.0–1.0) for speculative detection methods like
    /// cross-block MEV. None = standard on-chain detected opportunity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// Build a canonical dedup string from the opportunity's key fields (L9).
/// Exposed as a free function so the runner can assign IDs after collection.
pub fn compute_canonical_id(
    strategy: Strategy,
    _block: u64,
    pool_a: Address,
    pool_b: Address,
    token_in: Address,
    token_out: Address,
    victim_tx: Option<usize>,
    backrun_tx: Option<usize>,
) -> String {
    match strategy {
        Strategy::Sandwich => {
            format!("Sandwich|{:#x}|victim:{:?}|backrun:{:?}", pool_a, victim_tx, backrun_tx)
        }
        _ => {
            format!(
                "{:?}|{:#x}|{:#x}|{:#x}|{:#x}",
                strategy, pool_a, pool_b, token_in, token_out,
            )
        }
    }
}

impl MevOpportunity {
    /// Create a new MEV opportunity with required fields.
    /// Strategy-specific fields should be set via builder methods.
    pub fn new(
        block_number: u64,
        tx_index: usize,
        strategy: Strategy,
        pool_a: Address,
        timestamp: u64,
    ) -> Self {
        MevOpportunity {
            canonical_id: None,
            block_number,
            tx_index,
            strategy,
            pool_a,
            pool_b: Address::ZERO,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            input_amount: U256::ZERO,
            expected_profit: U256::ZERO,
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            pga_adjusted_profit: None,
            gas_cost_wei: 0,
            timestamp,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
        }
    }

    /// Compute and store the canonical dedup ID (L9) based on strategy type
    /// and key identifying fields. Returns self for builder chaining.
    pub fn with_canonical_id(mut self) -> Self {
        self.canonical_id = Some(compute_canonical_id(
            self.strategy,
            self.block_number,
            self.pool_a,
            self.pool_b,
            self.token_in,
            self.token_out,
            self.victim_tx_index,
            self.backrun_tx_index,
        ));
        self
    }

    /// Set JIT-specific fields: tick range and liquidity amount.
    /// Panics in debug builds if strategy is not JIT or JitArb.
    pub fn with_jit_fields(mut self, tick_lower: i32, tick_upper: i32, liquidity: u128) -> Self {
        debug_assert!(
            self.strategy == Strategy::Jit || self.strategy == Strategy::JitArb,
            "JIT fields only valid for Jit/JitArb strategies"
        );
        self.tick_lower = Some(tick_lower);
        self.tick_upper = Some(tick_upper);
        self.liquidity_amount = Some(liquidity);
        self
    }

    /// Set sandwich-specific fields: victim and backrun tx indices.
    /// Panics in debug builds if strategy is not Sandwich.
    pub fn with_sandwich_fields(mut self, victim_tx_index: usize, backrun_tx_index: usize) -> Self {
        debug_assert!(
            self.strategy == Strategy::Sandwich,
            "Sandwich fields only valid for Sandwich strategy"
        );
        self.victim_tx_index = Some(victim_tx_index);
        self.backrun_tx_index = Some(backrun_tx_index);
        self
    }

    /// Set multi-hop path.
    /// Panics in debug builds if strategy is not MultiHopArb.
    pub fn with_path(mut self, path: Vec<Address>) -> Self {
        debug_assert!(
            self.strategy == Strategy::MultiHopArb,
            "Path only valid for MultiHopArb strategy"
        );
        self.path = Some(path);
        self
    }
}

/// Saved results file wrapping opportunities with run metadata.
///
/// Written to `export_path` and re-read by the `report` subcommand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultsFile {
    pub run_id: String,
    pub chain: String,
    pub start_block: u64,
    pub end_block: u64,
    pub range_mode: String,
    pub strategies: Vec<String>,
    pub flash_loan_provider: String,
    pub resolved_at: u64,
    pub created_at: u64,
    pub opportunities: Vec<MevOpportunity>,
    /// Competition analysis results (optional, only populated when --competition is enabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub competition: Option<CompetitionReport>,
}


