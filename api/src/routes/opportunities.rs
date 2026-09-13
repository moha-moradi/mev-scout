//! `GET /api/opportunities` — scanner detections written by `run`/`live`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use mev_scout_core::explorer::store::{OpportunityRow, OpportunityRunSummary};

use crate::error::ApiResult;
use crate::pagination::{paginate, Paginated};
use crate::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/opportunities", get(list))
        .route("/api/opportunities/runs", get(run_summaries))
}

#[derive(Deserialize)]
pub struct OppQuery {
    pub run_id: Option<String>,
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

async fn list(
    State(state): State<SharedState>,
    Query(q): Query<OppQuery>,
) -> ApiResult<Json<Paginated<OpportunityRow>>> {
    let limit = q.limit.unwrap_or(50).min(500);
    let offset = q.offset.unwrap_or(0);
    let chain = state.active_chain().await;
    // Block window: explicit from/to, else the whole table.
    let (from, to) = (q.from.unwrap_or(0), q.to.unwrap_or(u64::MAX / 4));

    let rows = {
        let conn = state.explorer_conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT run_id, block_number, tx_index, strategy, pool_a, pool_b,
                    token_in, token_out, expected_profit, mempool_only,
                    detection_path, canonical_id, tx_hash
             FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3
             ORDER BY block_number",
        )?;
        let mapped = stmt.query_map(
            rusqlite::params![chain.to_string(), from as i64, to as i64],
            |r| {
                Ok(OpportunityRow {
                    run_id: r.get(0)?,
                    block_number: r.get::<_, i64>(1)? as u64,
                    tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    strategy: r.get(3)?,
                    pool_a: r.get(4)?,
                    pool_b: r.get(5)?,
                    token_in: r.get(6)?,
                    token_out: r.get(7)?,
                    expected_profit: r.get(8)?,
                    mempool_only: r.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
                    detection_path: r.get(10)?,
                    canonical_id: r.get(11)?,
                    tx_hash: r.get(12)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in mapped {
            out.push(r?);
        }
        out
    };

    // run_id filter applied in the API layer (column-level filter kept
    // simple; row counts per run are modest).
    let filtered: Vec<OpportunityRow> = rows
        .into_iter()
        .filter(|r| match &q.run_id {
            Some(rid) => r.run_id.as_deref() == Some(rid.as_str()),
            None => true,
        })
        .collect();
    Ok(Json(paginate(filtered, offset, limit)))
}

/// Distinct run_ids in `opportunities`, newest first — includes `live_*`
/// runs without manifests. Powers run-id dropdowns.
async fn run_summaries(
    State(state): State<SharedState>,
) -> ApiResult<Json<Vec<OpportunityRunSummary>>> {
    let conn = state.explorer_conn.lock().await;
    let mut stmt = conn.prepare(
        "SELECT run_id, COUNT(*), MIN(timestamp), MAX(timestamp)
         FROM opportunities
         WHERE run_id IS NOT NULL
         GROUP BY run_id
         ORDER BY MAX(timestamp) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(OpportunityRunSummary {
            run_id: r.get(0)?,
            count: r.get::<_, i64>(1)? as u64,
            first_ts: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
            last_ts: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    drop(stmt);
    drop(conn);
    Ok(Json(out))
}

// Silence unused Path import if not needed elsewhere.
#[allow(unused_imports)]
use Path as _PathGuard;
