//! Route registration: merges all sub-routers under `/api`.

pub mod chains;
pub mod config;
pub mod explorer;
pub mod health;
pub mod jobs;
pub mod opportunities;
pub mod pools;
pub mod results;
pub mod runs;
pub mod sync;

use axum::Router;

use crate::state::SharedState;

pub fn api_router() -> Router<SharedState> {
    Router::new()
        .merge(health::router())
        .merge(chains::router())
        .merge(config::router())
        .merge(explorer::router())
        .merge(results::router())
        .merge(runs::router())
        .merge(opportunities::router())
        .merge(pools::router())
        .merge(sync::router())
        .merge(jobs::router())
}
