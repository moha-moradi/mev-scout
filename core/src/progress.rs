//! Typed job-progress plumbing shared by every host of the engine: the CLI
//! binary (presentation sinks) and the API job runner (recording sinks).
//! Commands emit stage events through a [`JobProgress`] sink and honor
//! cooperative cancellation via [`JobProgress::cancelled`].

use serde::{Deserialize, Serialize};

/// One stage-progress event emitted by the engine's long-running paths.
/// Fields mirror the historical NDJSON `--progress json` contract so the
/// CLI/API JSON surface stays stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ops: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl ProgressEvent {
    pub fn stage(stage: &str) -> ProgressEvent {
        ProgressEvent {
            stage: stage.to_string(),
            done: None,
            total: None,
            run_id: None,
            ops: None,
            elapsed_ms: None,
        }
    }

    pub fn pct(&self) -> Option<f64> {
        match (self.done, self.total) {
            (Some(d), Some(t)) if t > 0 => Some(d as f64 / t as f64),
            _ => None,
        }
    }
}

/// Progress sink injected into orchestration. The plain CLI passes a
/// presentation sink; the API passes a sink that records events + logs and
/// honours cancellation.
pub trait JobProgress: Send + Sync {
    /// Consume a stage event.
    fn emit(&self, evt: ProgressEvent);
    /// Append a human-readable line to the job log.
    fn log(&self, line: &str);
    /// True once a cooperative stop has been requested.
    fn cancelled(&self) -> bool;
}

/// Discarding sink for synchronous API handlers that only need the outcome.
pub struct NoopProgress;

impl JobProgress for NoopProgress {
    fn emit(&self, _evt: ProgressEvent) {}
    fn log(&self, _line: &str) {}
    fn cancelled(&self) -> bool {
        false
    }
}