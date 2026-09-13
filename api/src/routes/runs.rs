//! `GET /api/runs` — backtest execution history (run manifests).

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use mev_scout_core::cache::RunManifest;

use crate::error::ApiResult;
use crate::pagination::{paginate, Paginated};
use crate::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/runs", get(runs))
}

#[derive(Deserialize)]
pub struct RunsQuery {
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

async fn runs(
    State(state): State<SharedState>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<Json<Paginated<RunManifest>>> {
    let limit = q.limit.unwrap_or(50).min(100);
    let offset = q.offset.unwrap_or(0);
    let manifests = {
        let conn = state.cache_conn.lock().await;
        crate::routes::results::query_manifests(&conn)?
    };
    Ok(Json(paginate(manifests, offset, limit)))
}
