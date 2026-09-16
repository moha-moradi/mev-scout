//! mev-scout command library. The `mev-scout` binary is a thin wrapper over
//! these modules; the API links this crate and drives the same command
//! orchestration in-process (with a typed [`job_progress::JobProgress`] sink).

pub mod cli;
pub mod commands;
pub mod display;
pub mod job_progress;
pub mod overrides;
pub mod rpc_setup;

pub use job_progress::{JobProgress, ProgressEvent};