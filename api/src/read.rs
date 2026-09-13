//! Read helpers shared by route handlers: thin wrappers that open a
//! temporary `ExplorerStore` over the active DB file (the store owns its
//! `rusqlite::Connection`, so state's raw read-only connections can't be
//! reused for its methods).

use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;

/// Reconstructed `MevOpportunity` list for a run (same reconstruction
/// `cli report` uses).
pub async fn opportunities_by_run(
    state: &SharedState,
    run_id: &str,
) -> ApiResult<Vec<mev_scout_core::types::MevOpportunity>> {
    let path = state.explorer_db_path.read().await.clone();
    let store = mev_scout_core::explorer::store::ExplorerStore::open(&path)
        .map_err(ApiError::internal)?;
    store
        .opportunities_by_run(run_id)
        .map_err(ApiError::internal)
}
