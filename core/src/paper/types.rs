//! Paper-session data contracts (virtual-fund bot P&L).

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

/// Why a detected opportunity was not taken as a paper fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FillSkipReason {
    /// Profit is not native-normalized (e.g. liquidation).
    NotNativeUnit,
    /// `expected_profit <= gas_cost_wei`.
    NonPositiveNet,
    /// Gas cost exceeds wallet surplus (`wallet - reserve_wei`).
    InsufficientGas,
    /// Shares a pool with an already-accepted fill in the same block.
    PoolConflict,
    /// Hit `max_fills_per_block` safety cap.
    MaxFillsPerBlock,
}

impl FillSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotNativeUnit => "not_native_unit",
            Self::NonPositiveNet => "non_positive_net",
            Self::InsufficientGas => "insufficient_gas",
            Self::PoolConflict => "pool_conflict",
            Self::MaxFillsPerBlock => "max_fills_per_block",
        }
    }
}

impl std::fmt::Display for FillSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Paper session mode for persistence / CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaperMode {
    Run,
    Live,
    Sim,
}

impl PaperMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Live => "live",
            Self::Sim => "sim",
        }
    }
}

impl std::fmt::Display for PaperMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One accepted paper fill (virtual execution).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperFill {
    pub block_number: u64,
    pub tx_index: Option<usize>,
    pub canonical_id: Option<String>,
    pub strategy: String,
    pub gross_wei: u128,
    pub gas_wei: u128,
    pub net_wei: i128,
    pub wallet_before: u128,
    pub wallet_after: u128,
    pub pools: Vec<Address>,
    pub mempool_only: bool,
}

/// One skipped candidate with reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSkip {
    pub block_number: u64,
    pub tx_index: Option<usize>,
    pub canonical_id: Option<String>,
    pub strategy: String,
    pub reason: FillSkipReason,
}

/// Result of applying the ledger over a batch of opportunities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerResult {
    pub starting_gas_wei: u128,
    pub ending_gas_wei: u128,
    pub reserve_wei: u128,
    /// Peak drop from a running high-water mark of the wallet (wei).
    pub max_drawdown_wei: u128,
    pub fills: Vec<PaperFill>,
    pub skips: Vec<PaperSkip>,
    pub gross_wei: u128,
    pub gas_wei: u128,
    pub net_profit_wei: i128,
    pub best_net_wei: i128,
    pub start_block: Option<u64>,
    pub end_block: Option<u64>,
}

impl LedgerResult {
    pub fn fills_count(&self) -> usize {
        self.fills.len()
    }

    pub fn skips_count(&self) -> usize {
        self.skips.len()
    }
}

/// Persisted paper session row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSession {
    pub session_id: String,
    pub chain: String,
    pub mode: String,
    pub linked_run_id: Option<String>,
    pub start_block: u64,
    pub end_block: u64,
    pub starting_gas_wei: String,
    pub ending_gas_wei: String,
    pub reserve_wei: String,
    pub fills: u64,
    pub skipped: u64,
    pub net_profit_wei: String,
    pub max_drawdown_wei: String,
    pub created_at: u64,
}
