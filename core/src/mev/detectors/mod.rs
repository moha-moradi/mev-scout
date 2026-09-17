mod arb_common;
pub mod jit;
pub mod jit_arb;
pub mod liquidation;
pub mod mempool;
pub mod multi_hop;
pub mod sandwich;
pub mod two_hop;

/// How an opportunity was detected — keeps the DB/`Option<String>` boundary
/// as a stable string while call sites use a typed tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    pub fn to_owned_string(self) -> String {
        self.as_str().to_string()
    }
}

/// Detection-path tag for opportunities found by replay/backtest.
pub const REPLAY_PATH: &str = DetectionPath::Replay.as_str();
/// Detection-path tag for opportunities found on the (pending) mempool.
pub const PENDING_PATH: &str = DetectionPath::Pending.as_str();
/// Detection-path tag for log-only synthesis in live mode.
pub const LOG_ONLY_PATH: &str = DetectionPath::LogOnly.as_str();

pub use jit::JitDetector;
pub use jit_arb::JitArbDetector;
pub use liquidation::{
    compute_health_factor, AaveReserveCache, AaveReserveData, LiquidationDetector,
};
pub use mempool::{capture_pending_block, detect_pending_opportunities, PendingBlockCapture};
pub use multi_hop::MultiHopArbDetector;
pub use sandwich::SandwichDetector;
pub use two_hop::{balancer_quote_exact_in, curve_output_amount, quote_path, TwoHopArbDetector};
