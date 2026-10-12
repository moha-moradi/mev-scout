//! Historical explorer backfill outcome type shared with the CLI backfill path.
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct BackfillOutcome {
    pub from_block: u64,
    pub to_block: u64,
    pub blocks_processed: u64,
    pub ops: u64,
    pub elapsed: Duration,
}
