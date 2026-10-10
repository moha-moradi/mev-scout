//! Shared job orchestration for the CLI host.
mod backfill;
mod discover;
mod explorer_validate;
mod index;
mod live;
mod report;
mod rpc;
mod run;
mod tokens;
mod trace;

pub use backfill::{job_backfill, BackfillOpts, BackfillOutcome};
pub use discover::{job_discover, DiscoverOpts, DiscoverOutcome};
pub use explorer_validate::{job_explorer_validate, ExplorerValidateOpts, ExplorerValidateOutcome};
pub use index::load_pool_registry;
pub use index::{job_index, IndexOpts, IndexOutcome, PoolRegistry};
pub use live::{job_live, LiveLoopOutcome, LiveOneShotOutcome, LiveOpts, LiveOutcome};
pub use report::{job_report, ReportOpts, ReportOutcome};
pub use rpc::{init_rpc, RpcSetup};
pub use run::{job_run, RunOpts, RunOutcome};
pub use tokens::{job_tokens, TokenEntry, TokensOpts, TokensOutcome};
pub use trace::{job_trace_op, summarize_prestatediff, trace_verdict, TraceOutcome, TraceVerdict};
