//! `GET /api/sync` — sync state for the active chain (graceful when missing).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::error::ApiResult;
use crate::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/sync", get(sync_state))
}

#[derive(Serialize, Default)]
pub struct SyncResponse {
    /// Highest block the explorer has indexed for the active chain.
    pub explorer_head: u64,
    /// Highest block recorded in the scanner cache manifests (approx head).
    pub cache_head: u64,
    /// Last indexed_at timestamp (unix seconds).
    pub last_indexed: Option<u64>,
}

async fn sync_state(State(state): State<SharedState>) -> ApiResult<Json<SyncResponse>> {
    let chain = state.active_chain().await;
    let chain_id = chain.chain_id();

    let explorer = {
        let conn = state.explorer_conn.lock().await;
        conn.query_row(
            "SELECT head, indexed_to, last_indexed_at FROM sync_state WHERE chain_id = ?1",
            rusqlite::params![chain_id as i64],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                ))
            },
        )
        .optional()
    };
    let cache_head = {
        let conn = state.cache_conn.lock().await;
        conn.query_row(
            "SELECT COALESCE(MAX(end_block), 0) FROM run_manifests WHERE chain = ?1",
            rusqlite::params![chain.to_string()],
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v as u64)
        .unwrap_or(0)
    };

    let (explorer_head, _indexed_to, last_indexed) = explorer.unwrap_or((0, 0, None));
    Ok(Json(SyncResponse {
        explorer_head,
        cache_head,
        last_indexed,
    }))
}

trait OptionalRow {
    fn optional(self) -> Option<(u64, u64, Option<u64>)>;
}

impl OptionalRow for rusqlite::Result<(u64, u64, Option<u64>)> {
    fn optional(self) -> Option<(u64, u64, Option<u64>)> {
        self.ok()
    }
}
