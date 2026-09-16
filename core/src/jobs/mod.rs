//! Shared job orchestration for CLI and API hosts.

mod discover;
mod index;
mod live;
mod report;
mod rpc;
mod run;
mod scan;
mod tokens;

pub use discover::{job_discover, DiscoverOpts, DiscoverOutcome};
pub use index::{job_index, IndexOpts, IndexOutcome};
pub use live::{job_live, LiveLoopOutcome, LiveOneShotOutcome, LiveOpts, LiveOutcome};
pub use report::{job_report, ReportOpts, ReportOutcome};
pub use rpc::{init_rpc, RpcSetup};
pub use run::{job_run, RunOpts, RunOutcome};
pub use scan::{job_scan, ScanKind, ScanOpts, ScanOutcome};
pub use tokens::{job_tokens, TokensOpts, TokensOutcome};
