//! Paper-session data contracts (virtual-fund bot P&L).
use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

/// Why a detected opportunity was not taken as a paper fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FillSkipReason {
    /// Profit is not native-normalized.
    NotNativeUnit,
    /// `expected_profit <= gas_cost_wei`.
    NonPositiveNet,
    /// Gas cost exceeds wallet surplus (`wallet - reserve_wei`).
    InsufficientGas,
    /// Shares a pool with an already-accepted fill in the same block.
    PoolConflict,
    /// Hit `max_fills_per_block` safety cap.
    MaxFillsPerBlock,
    /// State snapshot missing for this candidate under `--sim state`.
    SimNotQuotable,
    /// Re-quoted net ≤ 0 after earlier fills moved the market.
    SimPriceConsumed,
    /// `out < in + flash fee` after re-quote.
    SimRepayShortfall,
    /// A leg's pool is absent from the snapshot or not simulatable (non-V2/V3-family).
    SimPathUnavailable,
    /// Strategy is not state-simulatable (e.g. JIT under `--sim state`).
    SimStrategyUnsupported,
}

impl FillSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotNativeUnit => "not_native_unit",
            Self::NonPositiveNet => "non_positive_net",
            Self::InsufficientGas => "insufficient_gas",
            Self::PoolConflict => "pool_conflict",
            Self::MaxFillsPerBlock => "max_fills_per_block",
            Self::SimNotQuotable => "sim_not_quotable",
            Self::SimPriceConsumed => "sim_price_consumed",
            Self::SimRepayShortfall => "sim_repay_shortfall",
            Self::SimPathUnavailable => "sim_path_unavailable",
            Self::SimStrategyUnsupported => "sim_strategy_unsupported",
        }
    }
}

impl std::fmt::Display for FillSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Paper session mode for persistence / CLI.
///
/// Distinct from [`SimMode`]: this is the *job kind* that wrote the session
/// (`run` / `live` / legacy `sim` label). Do not overload with fill verification.
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

/// How paper fills were verified against pool state.
///
/// Distinct from [`PaperMode`]: this is fill-verification policy (`assumed` vs
/// `state`), not which job wrote the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SimMode {
    /// Credit detector `expected_profit − gas` without re-quoting (default).
    #[default]
    Assumed,
    /// Re-derive fill net from pool snapshots + liquidity consumption.
    StateSim,
}

impl SimMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assumed => "assumed",
            Self::StateSim => "state",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "assumed" => Some(Self::Assumed),
            "state" | "state-sim" | "statesim" => Some(Self::StateSim),
            _ => None,
        }
    }
}

impl std::fmt::Display for SimMode {
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
    /// Verification mode that produced this fill.
    pub sim_mode: SimMode,
    /// Native gross after sim (identity with `gross_wei` under `assumed`).
    pub sim_gross_wei: u128,
    /// Native net after sim (identity with `net_wei` under `assumed`).
    pub sim_net_wei: i128,
    /// Flash-loan borrow size used for repay check (`0` under assumed).
    pub borrowed_wei: u128,
    /// Whether `out ≥ borrowed + fee` held (`true` under assumed).
    pub repay_ok: bool,
    /// Overlay used a pool state written by an earlier fill (co-impact).
    pub state_drift: bool,
}

impl PaperFill {
    /// Fill sim_* fields as identity with claimed gross/net (assumed path).
    pub fn with_assumed_sim(mut self) -> Self {
        self.sim_mode = SimMode::Assumed;
        self.sim_gross_wei = self.gross_wei;
        self.sim_net_wei = self.net_wei;
        self.borrowed_wei = 0;
        self.repay_ok = true;
        self.state_drift = false;
        self
    }
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
    /// Session verification mode (defaults to Assumed for legacy callers).
    #[serde(default)]
    pub sim_mode: SimMode,
    /// Blocks scanned via full revm replay (optional session counter).
    #[serde(default)]
    pub replay_blocks: u64,
    /// Blocks scanned via log-only sync (optional session counter).
    #[serde(default)]
    pub log_only_blocks: u64,
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
    pub sim_mode: Option<String>,
    pub replay_blocks: Option<u64>,
    pub log_only_blocks: Option<u64>,
}
