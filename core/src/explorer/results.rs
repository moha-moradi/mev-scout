//! Persistence of scanner run results (opportunities + rejections) into the
//! explorer store. Shared by the CLI (`run`/`live`) and the API jobs so the
//! results layer (which feeds `explorer validate`) lives in one place.

use alloy::primitives::{Address, U256};

use crate::config::Config;
use crate::explorer::store::{ExplorerStore, OpportunityInput};
use crate::explorer::RejectedCandidate;
use crate::types::{ChainName, ResultsFile};

/// Persist run/live results into the explorer store's `opportunities` table.
/// Failures here warn only — the execution history still lives in SQLite.
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
    for opp in &results_file.opportunities {
        let path_str = opp.path.as_ref().map(|p| {
            p.iter()
                .map(|a| format!("{a:#x}"))
                .collect::<Vec<_>>()
                .join(",")
        });
        let confidence_str = opp.confidence.map(|c| format!("{c:.2}"));
        if let Err(e) = store.insert_opportunity(OpportunityInput {
            run_id,
            chain: &results_file.chain,
            block_number: opp.block_number,
            tx_index: Some(opp.tx_index as u64),
            strategy: &opp.strategy.to_string(),
            pool_a: Some(opp.pool_a),
            pool_b: (opp.pool_b != Address::ZERO).then_some(opp.pool_b),
            token_in: (!opp.token_in.is_zero()).then_some(opp.token_in),
            token_out: (!opp.token_out.is_zero()).then_some(opp.token_out),
            input_amount: Some(opp.input_amount),
            expected_profit: Some(opp.expected_profit),
            gas_cost_wei: Some(U256::from(opp.gas_cost_wei)),
            path: path_str.as_deref(),
            timestamp: Some(opp.timestamp),
            mempool_only: opp.mempool_only,
            confidence: confidence_str.as_deref(),
            sender: opp.sender,
            tx_hash: opp.tx_hash,
            detection_path: opp.detection_path.as_deref(),
            canonical_id: opp.canonical_id.as_deref(),
        }) {
            tracing::warn!("opportunities-table insert failed: {e}");
        }
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