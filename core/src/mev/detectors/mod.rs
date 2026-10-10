mod arb_common;
pub mod backrun;
pub(crate) mod ctx;
pub mod jit;
pub(crate) mod mempool;
pub mod multi_hop;
pub mod two_hop;

pub use crate::types::DetectionPath;
pub use ctx::DetectCtx;

/// Detection-path tag for opportunities found by replay/backtest.
pub const REPLAY_PATH: &str = DetectionPath::Replay.as_str();
/// Detection-path tag for opportunities found on the (pending) mempool.
pub const PENDING_PATH: &str = DetectionPath::Pending.as_str();
/// Detection-path tag for log-only synthesis in live mode.
pub const LOG_ONLY_PATH: &str = DetectionPath::LogOnly.as_str();

pub use backrun::BackrunDetector;
pub use jit::JitDetector;
pub use mempool::{capture_pending_block, detect_pending_opportunities, PendingBlockCapture};
pub use multi_hop::MultiHopArbDetector;
pub use two_hop::{balancer_quote_exact_in, curve_output_amount, quote_path, TwoHopArbDetector};
