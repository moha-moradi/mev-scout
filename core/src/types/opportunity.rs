//! Data contracts for detected MEV opportunities and persisted result files.
//!
//! These types are the serialization boundary between the core backtest engine,
//! the CLI output layer, and the API serialization layer.

use crate::types::strategy::Strategy;
use alloy::primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

/// How an opportunity was detected — stable string at the DB/JSON boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionPath {
    /// Full EVM replay / backtest (`run`).
    Replay,
    /// Pending-mempool capture.
    Pending,
    /// Log-only synthesis (`live` without full replay).
    LogOnly,
}

impl DetectionPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Pending => "pending",
            Self::LogOnly => "log_only",
        }
    }
}

impl std::fmt::Display for DetectionPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DetectionPath {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "replay" => Ok(Self::Replay),
            "pending" => Ok(Self::Pending),
            "log_only" => Ok(Self::LogOnly),
            other => Err(format!("unknown detection_path '{other}'")),
        }
    }
}

/// A detected MEV opportunity from backtesting.
///
/// Different strategies populate different optional fields:
/// - `path` for multi-hop strategies,
/// - `tick_lower`/`tick_upper`/`liquidity_amount` for JIT strategies.
///
/// For `Strategy::Backrun`, `sender` / `tx_hash` / `tx_index` denote the
/// **anchor** (victim) transaction, not a searcher transaction;
/// `victim_tx_index` carries the same index semantically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevOpportunity {
    /// Canonical dedup ID (L9): derived from strategy + key fields to uniquely
    /// identify this opportunity across detectors and aggregation passes.
    /// Example: "TwoHopArb|0xaaa|0xbbb|0xccc|0xddd".
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
    /// Transaction index of the victim swap. For `Strategy::Backrun`, this
    /// identifies the anchor transaction that triggered the backrun opportunity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victim_tx_index: Option<usize>,
    /// Transaction index of the backrun. For regular backtests, this is typically
    /// `None` (backrun candidates are not observed as realized searcher txs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backrun_tx_index: Option<usize>,
    /// Whether this opportunity was detected from mempool/pending transactions
    /// rather than settled on-chain blocks.
    #[serde(default)]
    pub mempool_only: bool,
    /// Confidence score (0.0–1.0) for speculative detection methods.
    /// None = standard on-chain detected opportunity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Transaction sender (EOA `from`) when known. Populated by the runner
    /// stamping pass; enables sender-based joins and explorer sender metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<Address>,
    /// On-chain transaction hash when the opportunity is anchored to a
    /// specific transaction (replay path). Enables tx-granularity matching
    /// against realized explorer ops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<B256>,
    /// How this opportunity was detected (`replay` / `log_only` / `pending`).
    /// Makes run-vs-live coverage gaps attributable to detection path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_path: Option<DetectionPath>,
}

/// Key fields used to build a canonical opportunity dedup id.
#[derive(Debug, Clone, Copy)]
pub struct CanonicalIdParts {
    pub strategy: Strategy,
    pub pool_a: Address,
    pub pool_b: Address,
    pub token_in: Address,
    pub token_out: Address,
}

/// Build a canonical dedup string from the opportunity's key fields (L9).
pub fn compute_canonical_id(parts: CanonicalIdParts) -> String {
    format!(
        "{:?}|{:#x}|{:#x}|{:#x}|{:#x}",
        parts.strategy, parts.pool_a, parts.pool_b, parts.token_in, parts.token_out,
    )
}

/// Build a canonical id for backrun opportunities that matches the explorer's form.
pub fn compute_backrun_canonical_id(anchor_pool: Address, victim_tx_index: usize) -> String {
    format!("Backrun|{anchor_pool:#x}|source_tx:{victim_tx_index}")
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
            sender: None,
            tx_hash: None,
            detection_path: None,
        }
    }
}

/// Saved results file wrapping opportunities with run metadata.
///
/// Built during `run`/`live` for persistence into the explorer SQLite store,
/// and reconstructed by the `report` subcommand from stored rows.
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
}
