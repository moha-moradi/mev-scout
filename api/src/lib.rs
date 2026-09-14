//! mev-scout API library crate — shares logic between the binary and the
//! integration tests in `tests/`.

pub mod error;
pub mod jobs;
pub mod pagination;
pub mod read;
pub mod routes;
pub mod state;

use axum::routing::get;
use axum::Router;

use state::SharedState;

/// Build the full application router (API routes only; static-file serving
/// is wired in the binary). Shared by `main.rs` and the test harness.
pub fn api_router() -> Router<SharedState> {
    routes::api_router()
}

/// Build a test/embedded router with the top-level `/api` index route.
/// `with_state` provides the `SharedState`, yielding a `Router<()>` that
/// implements `tower::Service` (required by `tower::ServiceExt::oneshot`).
pub fn test_router(state: SharedState) -> Router<()> {
    Router::new()
        .route("/api", get(api_index))
        .merge(api_router())
        .with_state(state)
}

async fn api_index() -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({
            "name": "mev-scout-api"
        })),
    )
}