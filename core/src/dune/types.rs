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

// ── Pool Discovery Types ───────────────────────────────────────────────

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

/// Token metadata from Dune's `tokens.erc20` dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneTokenInfo {
    pub contract_address: Address,
    pub symbol: String,
    pub decimals: u8,
    pub name: Option<String>,
}

/// Pool metadata with token symbols/decimals for display/reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DunePoolWithMetadata {
    pub pool_address: Address,
    pub token0_address: Address,
    pub token1_address: Address,
    pub token0_symbol: String,
    pub token1_symbol: String,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub fee: u32,
    pub project: String,
    pub creation_block: u64,
}

// ── Trade & Swap Types ─────────────────────────────────────────────────

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

/// A large-value swap detected on Dune.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneLargeSwap {
    pub block_number: u64,
    pub tx_hash: String,
    pub pool_address: Address,
    pub token_in_symbol: String,
    pub token_out_symbol: String,
    pub amount_usd: f64,
    pub amount_token: String,
    pub taker: Address,
    pub block_time: String,
}

// ── MEV Detection Types ────────────────────────────────────────────────

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

/// A liquidation event from Dune (Aave V3 / Compound).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneLiquidation {
    pub block_number: u64,
    pub tx_hash: String,
    pub protocol: String,
    pub user: Address,
    pub liquidator: Address,
    pub collateral_token: Address,
    pub debt_token: Address,
    pub collateral_amount: String,
    pub debt_amount: String,
    pub amount_usd: Option<f64>,
    pub block_time: Option<String>,
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

/// Hourly gas price stats for gas optimization (Cheapest periods).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneGasByHour {
    pub hour: String,
    pub avg_gas_price_gwei: f64,
    pub min_gas_price_gwei: f64,
    pub max_gas_price_gwei: f64,
    pub median_gas_price_gwei: Option<f64>,
    pub tx_count: u64,
}

/// A large token transfer (whale movement) — leading indicator for volatility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneWhaleTransfer {
    pub block_number: u64,
    pub tx_hash: String,
    pub symbol: String,
    pub amount: f64,
    pub amount_usd: f64,
    pub from_address: Address,
    pub to_address: Address,
    pub block_time: String,
}

/// Cross-chain bridge transfer volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneBridgeFlow {
    pub blockchain: String,
    pub total_bridged_usd: f64,
    pub tx_count: u64,
    pub from_time: String,
    pub to_time: String,
}

/// Net cross-chain bridge flow (inflow - outflow).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneBridgeNetFlow {
    pub blockchain: String,
    pub total_inflow_usd: f64,
    pub total_outflow_usd: f64,
    pub net_flow_usd: f64,
    pub tx_count: u64,
}

// ── Block & Gas Types ──────────────────────────────────────────────────

/// Block metadata from Dune.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneBlockInfo {
    pub block_number: u64,
    pub block_time: String,
    pub timestamp_utc: String,
    pub gas_used: Option<u64>,
    pub gas_limit: Option<u64>,
    pub base_fee_per_gas: Option<f64>,
    pub tx_count: Option<u32>,
}

/// Gas price snapshot for a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneGasPrice {
    pub block_number: u64,
    pub block_time: String,
    pub base_fee_gwei: f64,
    pub p25_gwei: Option<f64>,
    pub p50_gwei: Option<f64>,
    pub p75_gwei: Option<f64>,
    pub p95_gwei: Option<f64>,
    pub p99_gwei: Option<f64>,
}

// ── Failed Transaction Types ───────────────────────────────────────────

/// A failed/reverted transaction that carried value (potential MEV signal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneFailedTx {
    pub block_number: u64,
    pub tx_hash: String,
    pub value_eth: f64,
    pub from_address: Address,
    pub to_address: Option<Address>,
    pub gas_used: u64,
    pub gas_price_gwei: f64,
    pub error_reason: Option<String>,
}
