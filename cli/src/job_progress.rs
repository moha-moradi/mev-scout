//! Job progress sinks for the CLI binary. The shared trait/event types live
//! in `mev_scout_core::progress`; here we pick a presentation sink per mode:
//! indicatif bar for `live` and `explorer index`, noop otherwise.
//!
//! [`StdoutJsonProgress`] remains available for embedding hosts that want NDJSON.
pub use mev_scout_core::progress::{JobProgress, ProgressEvent};

/// Sink for the CLI's plain mode: fetch/backfill indicatif bar (or spinner
/// when total is unknown), other stages ignored.
#[derive(Default)]
pub struct BarProgress {
    bar: std::sync::Mutex<Option<indicatif::ProgressBar>>,
}

impl BarProgress {
    pub fn new() -> BarProgress {
        BarProgress::default()
    }

    fn ensure_bar(
        guard: &mut Option<indicatif::ProgressBar>,
        total: Option<u64>,
    ) -> &indicatif::ProgressBar {
        // `get_or_insert_with` never yields the absent case, so no `expect` is
        // needed to hand the bar back out.
        guard.get_or_insert_with(|| {
            let determinate = total.unwrap_or(0) > 0;
            if determinate {
                let pb = indicatif::ProgressBar::new(total.unwrap_or(0));
                #[allow(clippy::expect_used)]
                let style = indicatif::ProgressStyle::default_bar()
                    .template(
                        "[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({per_sec} b/s) {msg} ({eta})",
                    )
                    .expect("valid template")
                    .progress_chars("=> ");
                pb.set_style(style);
                pb
            } else {
                let pb = indicatif::ProgressBar::new_spinner();
                #[allow(clippy::expect_used)]
                let style = indicatif::ProgressStyle::default_spinner()
                    .template("[{elapsed_precise}] {spinner} {pos} blocks {msg}")
                    .expect("valid spinner template");
                pb.set_style(style);
                pb.enable_steady_tick(std::time::Duration::from_millis(100));
                pb
            }
        })
    }
}

impl JobProgress for BarProgress {
    fn emit(&self, evt: ProgressEvent) {
        match evt.stage.as_str() {
            "fetch" | "backfill" => {
                // Mutex poison only happens if another thread already panicked holding the bar.
                #[allow(clippy::unwrap_used)]
                let mut guard = self.bar.lock().unwrap();
                let pb = Self::ensure_bar(&mut guard, evt.total);
                if let Some(total) = evt.total {
                    if total > 0 {
                        pb.set_length(total);
                    }
                }
                if let Some(done) = evt.done {
                    pb.set_position(done);
                }
                if let Some(ops) = evt.ops {
                    pb.set_message(format!("ops={ops}"));
                }
            }
            "complete" =>
            {
                #[allow(clippy::unwrap_used)]
                if let Some(pb) = self.bar.lock().unwrap().take() {
                    pb.finish_and_clear();
                }
            }
            _ => {}
        }
    }

    fn log(&self, line: &str) {
        #[allow(clippy::unwrap_used)]
        let guard = self.bar.lock().unwrap();
        if let Some(pb) = guard.as_ref() {
            pb.println(line);
        } else {
            println!("{line}");
        }
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
