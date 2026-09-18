//! Job progress sinks for the CLI binary. The shared trait/event types live
//! in `mev_scout_core::progress` (the API job runner implements the same
//! trait over its own channels); here we pick a presentation sink per mode:
//! `--progress json` → NDJSON on stdout, otherwise an indicatif bar.
//!
//! The CLI reads `--progress json` to choose a [`StdoutJsonProgress`] sink,
//! falling back to [`BarProgress`].

pub use mev_scout_core::progress::{JobProgress, ProgressEvent};

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
