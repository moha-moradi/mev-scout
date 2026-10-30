//! Persistence of scanner run results (opportunities + rejections) into the
//! explorer store. Used by `live` and `job_run`.
use alloy::primitives::{Address, U256};

use crate::config::Config;
use crate::explorer::store::{ExplorerStore, OpportunityInput};
use crate::explorer::RejectedCandidate;
use crate::types::{ChainName, MevOpportunity, ResultsFile};

/// Owned projections of a [`MevOpportunity`] that [`OpportunityInput`] borrows.
///
/// The borrow must outlive the whole batch insert, so the computed strings (and
/// the converted gas value) are collected first instead of living in a
/// per-row temporary.
struct PreparedOpportunity<'a> {
    opp: &'a MevOpportunity,
    strategy: String,
    path: Option<String>,
    confidence: Option<String>,
    gas_cost_wei: U256,
}

/// Persist run/live results into the explorer store's `opportunities` table.
///
/// The run's rows are collected first and inserted in one transaction, so a
/// mid-run failure cannot leave a partial results set. Failures here warn only
/// — the execution history still lives in SQLite.
pub fn persist_opportunities_to_explorer(
    config: &Config,
    chain: ChainName,
    run_id: &str,
    results_file: &ResultsFile,
) {
    let store = match ExplorerStore::open(config.effective_explorer_db_path(&chain)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("explorer store open failed (results layer skipped): {e}");
            return;
        }
    };
    let prepared: Vec<PreparedOpportunity<'_>> = results_file
        .opportunities
        .iter()
        .map(|opp| PreparedOpportunity {
            opp,
            strategy: opp.strategy.to_string(),
            path: opp.path.as_ref().map(|p| {
                p.iter()
                    .map(|a| format!("{a:#x}"))
                    .collect::<Vec<_>>()
                    .join(",")
            }),
            confidence: opp.confidence.map(|c| format!("{c:.2}")),
            gas_cost_wei: U256::from(opp.gas_cost_wei),
        })
        .collect();
    let rows: Vec<OpportunityInput<'_>> = prepared
        .iter()
        .map(|p| OpportunityInput {
            run_id,
            chain: &results_file.chain,
            block_number: p.opp.block_number,
            tx_index: Some(p.opp.tx_index as u64),
            strategy: &p.strategy,
            pool_a: Some(p.opp.pool_a),
            pool_b: (p.opp.pool_b != Address::ZERO).then_some(p.opp.pool_b),
            token_in: (!p.opp.token_in.is_zero()).then_some(p.opp.token_in),
            token_out: (!p.opp.token_out.is_zero()).then_some(p.opp.token_out),
            input_amount: Some(p.opp.input_amount),
            expected_profit: Some(p.opp.expected_profit),
            gas_cost_wei: Some(p.gas_cost_wei),
            path: p.path.as_deref(),
            timestamp: Some(p.opp.timestamp),
            mempool_only: p.opp.mempool_only,
            confidence: p.confidence.as_deref(),
            sender: p.opp.sender,
            tx_hash: p.opp.tx_hash,
            detection_path: p.opp.detection_path.map(|p| p.as_str()),
            canonical_id: p.opp.canonical_id.as_deref(),
        })
        .collect();
    if let Err(e) = store.insert_opportunities(&rows) {
        tracing::warn!("opportunities-table insert failed: {e}");
    }
}

/// Persist drained runner rejections into the explorer store.
/// Only called when rejection recording is enabled.
pub fn persist_rejections_to_explorer(
    config: &Config,
    chain: ChainName,
    run_id: &str,
    rejections: &[RejectedCandidate],
) {
    if rejections.is_empty() {
        return;
    }
    let store = match ExplorerStore::open(config.effective_explorer_db_path(&chain)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("explorer store open failed (rejections skipped): {e}");
            return;
        }
    };
    let chain_str = chain.to_string();
    for r in rejections {
        if let Err(e) = store.insert_rejected_candidate(run_id, &chain_str, r) {
            tracing::warn!("rejected_candidates insert failed: {e}");
        }
    }
}
