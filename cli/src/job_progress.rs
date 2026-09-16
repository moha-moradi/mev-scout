//! Job progress plumbing shared by the CLI binary and embedding hosts (the
//! API). Commands emit typed stage events through a [`JobProgress`] sink; the
//! sink decides presentation (NDJSON to stdout, an indicatif bar, or a typed
//! channel) and exposes cooperative cancellation.
//!
//! The CLI reads `--progress json` to choose a [`StdoutJsonProgress`] sink,
//! falling back to [`BarProgress`]; embedding hosts implement the trait over
//! their own channels.

use serde::{Deserialize, Serialize};

/// One stage-progress event emitted by the commands.
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

/// Progress sink injected into command orchestration. The plain CLI passes a
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

/// Sink for the CLI's plain (non-`--progress json`) mode: a fetch-stage
/// indicatif bar, other stages ignored.
#[derive(Default)]
pub struct BarProgress {
    bar: std::sync::Mutex<Option<indicatif::ProgressBar>>,
}

impl BarProgress {
    pub fn new() -> BarProgress {
        BarProgress::default()
    }
}

impl JobProgress for BarProgress {
    fn emit(&self, evt: ProgressEvent) {
        match evt.stage.as_str() {
            "fetch" => {
                let mut guard = self.bar.lock().unwrap();
                if guard.is_none() {
                    let pb = indicatif::ProgressBar::new(evt.total.unwrap_or(0));
                    pb.set_style(
                        indicatif::ProgressStyle::default_bar()
                            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta})")
                            .expect("valid template")
                            .progress_chars("=> "),
                    );
                    *guard = Some(pb);
                }
                if let Some(pb) = guard.as_ref() {
                    if let Some(done) = evt.done {
                        pb.set_position(done);
                    }
                }
            }
            "complete" => {
                if let Some(pb) = self.bar.lock().unwrap().take() {
                    pb.finish_and_clear();
                }
            }
            _ => {}
        }
    }

    fn log(&self, line: &str) {
        println!("{line}");
    }

    fn cancelled(&self) -> bool {
        false
    }
}

/// Sink for the CLI's `--progress json` mode: emits each event as an NDJSON
/// line on stdout (same contract as the old hard-coded prints).
pub struct StdoutJsonProgress;

impl JobProgress for StdoutJsonProgress {
    fn emit(&self, evt: ProgressEvent) {
        if let Ok(line) = serde_json::to_string(&evt) {
            println!("{line}");
        }
    }

    fn log(&self, line: &str) {
        println!("{line}");
    }

    fn cancelled(&self) -> bool {
        false
    }
}

/// Sink that swallows events and never cancels (commands with no progress
/// output, e.g. `tokens`).
pub struct NoopProgress;

impl JobProgress for NoopProgress {
    fn emit(&self, _evt: ProgressEvent) {}
    fn log(&self, line: &str) {
        println!("{line}");
    }
    fn cancelled(&self) -> bool {
        false
    }
}