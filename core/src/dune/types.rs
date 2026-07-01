use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

/// Dune API error response.
#[derive(Debug, Deserialize)]
pub struct DuneApiError {
    pub error: String,
}

/// Generic column metadata from Dune results.
#[derive(Debug, Deserialize)]
pub struct DuneColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub col_type: String,
}

/// Execution state returned by Dune API.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExecutionState {
    Queued,
    Pending,
    Executing,
    Completed,
    Failed,
    Cancelled,
}

/// Execution metadata from Dune API.
#[derive(Debug, Deserialize)]
pub struct ExecutionMetadata {
    pub execution_id: String,
    pub query_id: Option<u64>,
    pub state: ExecutionState,
    #[serde(default)]
    pub submitted_at: String,
    #[serde(default)]
    pub expires_at: String,
    #[serde(default)]
    pub execution_started_at: Option<String>,
    #[serde(default)]
    pub execution_ended_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Execution response from execute endpoint.
#[derive(Debug, Deserialize)]
pub struct ExecutionResponse {
    pub execution_id: String,
}

/// A single row from Dune query results,
/// represented as a sequence of optional raw JSON values.
pub type DuneRow = Vec<Option<serde_json::Value>>;

/// Results from a completed Dune query execution.
#[derive(Debug, Deserialize)]
pub struct DuneResults {
    pub columns: Vec<DuneColumn>,
    pub rows: Vec<DuneRow>,
}

/// Full status+results response from Dune API.
#[derive(Debug, Deserialize)]
pub struct DuneExecutionResult {
    pub execution_id: String,
    pub state: ExecutionState,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub result: Option<DuneResults>,
}

// ── Dune row deserialization helpers ──────────────────────────────────────

/// A DEX pool discovered via Dune.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneDiscoveredPool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: Option<i32>,
    pub dex_label: String,
    pub creation_block: u64,
    pub factory: Option<Address>,
}

/// A single DEX trade from `dex.trades`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneTrade {
    pub block_number: u64,
    pub tx_hash: String,
    pub token_bought_address: Address,
    pub token_sold_address: Address,
    pub amount_bought: String,
    pub amount_sold: String,
    pub taker: Address,
    pub pool_address: Address,
    pub project: String,
    pub block_time: Option<String>,
}

/// A sandwich attack entry from Dune's curated dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneSandwich {
    pub block_number: u64,
    pub victim_tx_hash: String,
    pub front_tx_hash: String,
    pub back_tx_hash: String,
    pub sandwich_type: Option<String>,
    pub pool_address: Option<Address>,
}

/// Cross-validation result for a single MEV opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneCrossValidation {
    pub block_number: u64,
    pub tx_index: usize,
    pub strategy: String,
    /// Whether Dune confirmed the on-chain trade existed
    pub trade_confirmed: Option<bool>,
    /// Whether Dune confirmed a sandwich attack
    pub sandwich_confirmed: Option<bool>,
    /// The profit according to Dune price data
    pub dune_profit_usd: Option<f64>,
    /// Any error or info message from Dune lookups
    pub message: Option<String>,
}
