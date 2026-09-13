mod arb_common;
pub mod jit;
pub mod jit_arb;
pub mod liquidation;
pub mod mempool;
pub mod multi_hop;
pub mod sandwich;
pub mod two_hop;

/// Detection-path tag for opportunities found by replay/backtest — the
/// string typeclassifying a detection source is a bare literal everywhere
/// else, so both tags are centralized here.
pub const REPLAY_PATH: &str = "replay";
/// Detection-path tag for opportunities found on the (pending) mempool.
pub const PENDING_PATH: &str = "pending";

pub use jit::JitDetector;
pub use jit_arb::JitArbDetector;
pub use liquidation::{
    compute_health_factor, AaveReserveCache, AaveReserveData, LiquidationDetector,
};
pub use mempool::{capture_pending_block, detect_pending_opportunities, PendingBlockCapture};
pub use multi_hop::MultiHopArbDetector;
pub use sandwich::SandwichDetector;
pub use two_hop::{balancer_quote_exact_in, curve_output_amount, quote_path, TwoHopArbDetector};
