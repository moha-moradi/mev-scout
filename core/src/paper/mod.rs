//! Paper trading — virtual-fund bot P&L over detected opportunities.
//!
//! Distinct from `run`/`live` (detection) and `explorer` (realized MEV):
//! applies a gas-wallet ledger to answer "what would our theoretical P&L
//! have been?" without competition or on-chain execution.

pub mod ledger;
pub mod store;
pub mod types;

pub use ledger::{default_starting_gas_wei, is_native_eligible, LedgerPolicy, HARD_MAX_FILLS_PER_BLOCK};
pub use types::{
    FillSkipReason, LedgerResult, PaperFill, PaperMode, PaperSession, PaperSkip,
};
