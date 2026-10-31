//! Bounded classify-side reject sink.
//!
//! Anchored rows (`tx_index = Some`) are for candidates that already had a
//! plausible anchor. [`DropSink::bump`] collapses every other miss to one row
//! per block per `(reason, detail)` with `tx_index = None`. See
//! `RejectedCandidate`.
use std::collections::HashSet;

use crate::explorer::reject::{RejectReason, RejectedCandidate};

const STRATEGY: &str = "explorer_classify";

pub(super) struct DropSink {
    block: u64,
    rows: Vec<RejectedCandidate>,
    /// `(tx_index sentinel, reason, detail)` already emitted. Sentinel `-1`
    /// is the per-block aggregate.
    seen: HashSet<(i64, &'static str, &'static str)>,
}

impl DropSink {
    pub(super) fn new(block: u64) -> Self {
        Self {
            block,
            rows: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// One row for a candidate that already had an anchor (a tx index).
    pub(super) fn anchor(&mut self, tx_index: u64, reason: RejectReason, detail: &'static str) {
        self.push(Some(tx_index), reason, detail);
    }

    /// Fold an unanchored miss into the block's single row for this reason.
    pub(super) fn bump(&mut self, reason: RejectReason, detail: &'static str) {
        self.push(None, reason, detail);
    }

    fn push(&mut self, tx_index: Option<u64>, reason: RejectReason, detail: &'static str) {
        let key = (
            tx_index.map(|t| t as i64).unwrap_or(-1),
            reason.as_str(),
            detail,
        );
        if !self.seen.insert(key) {
            return;
        }
        self.rows.push(RejectedCandidate {
            block_number: self.block,
            tx_index,
            strategy: STRATEGY.to_string(),
            pool_a: None,
            pool_b: None,
            path: None,
            token_in: None,
            token_out: None,
            input_amount: None,
            expected_profit: None,
            expected_profit_usd: None,
            gas_cost_wei: None,
            reject_reason: reason.as_str().to_string(),
            detail: Some(detail.to_string()),
            created_at: crate::utils::epoch_secs(),
        });
    }

    pub(super) fn into_rejects(self) -> Vec<RejectedCandidate> {
        self.rows
    }
}
