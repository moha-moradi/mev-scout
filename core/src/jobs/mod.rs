//! Shared job orchestration for CLI and API hosts.

mod discover;
mod doctor;
mod explorer_validate;
mod export;
mod fetch;
mod index;
mod live;
mod replay;
mod report;
mod rpc;
mod run;
mod scan;
mod tokens;
mod trace;
mod validate_pools;

pub use discover::{job_discover, DiscoverOpts, DiscoverOutcome};
pub use doctor::{job_doctor, DoctorOutcome, ProviderProbe};
pub use explorer_validate::{job_explorer_validate, ExplorerValidateOpts, ExplorerValidateOutcome};
pub use export::{
    collect_export_ops, format_export_body, job_export, ExportOpts, ExportOutcome,
};
pub use fetch::{job_fetch, FetchOpts, FetchOutcome};
pub use index::{job_index, IndexOpts, IndexOutcome};
pub use live::{job_live, LiveLoopOutcome, LiveOneShotOutcome, LiveOpts, LiveOutcome};
pub use replay::{job_replay, ReplayOpts, ReplayOutcome, ReplayTxRow};
pub use report::{job_report, ReportOpts, ReportOutcome};
pub use rpc::{init_rpc, RpcSetup};
pub use run::{job_run, RunOpts, RunOutcome};
pub use scan::{job_scan, ScanKind, ScanOpts, ScanOutcome};
pub use tokens::{job_tokens, TokenEntry, TokensOpts, TokensOutcome};
pub use trace::{job_trace_op, summarize_prestatediff, TraceOutcome};
pub use validate_pools::{
    job_validate_pools, DexRecallRow, SourceReport, ValidatePoolsOpts, ValidatePoolsOutcome,
};
