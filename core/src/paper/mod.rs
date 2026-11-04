//! Paper trading — virtual-fund bot P&L over detected opportunities.
//!
//! Distinct from `run`/`live` (detection) and `explorer` (realized MEV):
//! applies a gas-wallet ledger to answer "what would our theoretical P&L
//! have been?" without competition or on-chain execution.

pub(crate) mod ledger;
pub(crate) mod recon;
pub(crate) mod sim;
pub(crate) mod store;
pub(crate) mod types;

pub use ledger::{
    default_starting_gas_wei, is_native_eligible, LedgerPolicy, HARD_MAX_FILLS_PER_BLOCK,
};
pub use recon::{paper_vs_executed, reconcile_sim_vs_exec, ReconFill, ReconReport, ReconVerdict};
pub use sim::{
    merge_seeds, pools_of_opp, promote_pending_seeds, push_pending_seeds, record_seeds_for_block,
    PendingSeed, PoolSnapshot, SeedMatchKey, SimFail, SimOutcome, SimPolicy, SimSeeds,
};
pub use types::{
    ExecAttempt, FillSkipReason, LedgerResult, PaperFill, PaperMode, PaperSession, PaperSkip,
    SimMode,
};
