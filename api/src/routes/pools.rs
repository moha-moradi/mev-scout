//! `GET /api/pools` — discovered pools with SQL-level filter/sort.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use mev_scout_core::pool::state::PoolInfo;

use crate::error::ApiResult;
use crate::pagination::Paginated;
use crate::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/pools", get(pools))
}

#[derive(Deserialize)]
pub struct PoolsQuery {
    pub q: Option<String>,
    pub dex: Option<String>,
    pub token: Option<String>,
    pub min_tvl: Option<f64>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

async fn pools(
    State(state): State<SharedState>,
    Query(q): Query<PoolsQuery>,
) -> ApiResult<Json<Paginated<PoolInfo>>> {
    let limit = q.limit.unwrap_or(200).min(1000);
    let offset = q.offset.unwrap_or(0);
    let order_desc = matches!(q.order.as_deref(), None | Some("desc"));

    // pools_filtered_paged is a SqliteStore method; open a temporary store
    // over the cache DB file (it owns its connection). Missing DB degrades
    // to an empty page (store open would fabricate one).
    let (items, total) = match state.open_cache_store_if_exists().await {
        Ok(store) => store
            .pools_filtered_paged(
                q.q.as_deref(),
                q.dex.as_deref(),
                q.token.as_deref(),
                q.min_tvl,
                q.sort.as_deref(),
                order_desc,
                offset,
                limit,
            )
            .map_err(crate::error::ApiError::internal)?,
        Err(_) => (Vec::new(), 0),
    };

    Ok(Json(Paginated {
        items,
        total,
        offset,
        limit,
    }))
}
