//! Integration tests: `/api/health` + `/api/chains` + missing-DB behavior.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use common::{test_router, test_state};

async fn get(app: &axum::Router<()>, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn health_reports_ok_fields() {
    let state = test_state().await;
    let app = test_router(state);
    let (status, json) = get(&app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("version").is_some());
    assert_eq!(json["chain"], "polygon");
    assert!(json["rpc_provider_count"].is_number());
    assert!(json["uptime_seconds"].is_number());
    // Job status shape.
    assert!(json["job_status"]["total"].is_number());
    assert!(json["job_status"]["running"].is_null());
    // DB paths point at nonexistent files in the harness → "missing".
    assert_eq!(json["db_status"]["cache"], "missing");
    assert_eq!(json["db_status"]["explorer"], "missing");
}

#[tokio::test]
async fn health_never_leaks_rpc_urls() {
    let state = test_state().await;
    let app = test_router(state);
    let (_, json) = get(&app, "/api/health").await;
    let raw = serde_json::to_string(&json).unwrap();
    assert!(!raw.contains("rpc_url"));
    assert!(!raw.contains("SECRETKEY"));
    assert!(!raw.contains("example.com"));
}

#[tokio::test]
async fn chains_returns_exactly_seven_with_correct_ids() {
    let state = test_state().await;
    let app = test_router(state);
    let (status, json) = get(&app, "/api/chains").await;
    assert_eq!(status, StatusCode::OK);
    let chains = json.as_array().unwrap();
    assert_eq!(chains.len(), 7);

    let expected: &[(&str, u64)] = &[
        ("polygon", 137),
        ("avalanche", 43114),
        ("bsc", 56),
        ("arbitrum", 42161),
        ("base", 8453),
        ("ethereum", 1),
        ("optimism", 10),
    ];
    for (name, id) in expected {
        let c = chains
            .iter()
            .find(|c| c["name"] == *name)
            .unwrap_or_else(|| panic!("missing chain {name}"));
        assert_eq!(c["chain_id"].as_u64(), Some(*id), "chain_id for {name}");
        assert!(c["has_cache_db"].is_boolean());
        assert!(c["has_explorer_db"].is_boolean());
    }
}

#[tokio::test]
async fn missing_db_read_endpoints_return_empty_not_500() {
    // A state with empty in-memory DBs (schema present, no rows) and file
    // paths pointing at nonexistent files → graceful empty responses.
    let state =
        common::state_with_conns(common::empty_explorer_db(), common::seeded_cache_db()).await;
    let app = test_router(state);
    for uri in [
        "/api/explorer/feed",
        "/api/explorer/stats",
        "/api/explorer/overview",
        "/api/explorer/top",
        "/api/explorer/ops",
        "/api/opportunities",
        "/api/opportunities/runs",
        "/api/sync",
        "/api/pools",
    ] {
        let (status, _) = get(&app, uri).await;
        assert!(
            status.is_success(),
            "{uri} should degrade gracefully, got {status}"
        );
    }
    let (status, _) = get(&app, "/api/explorer/op/0xdeadbeef").await;
    assert_eq!(status, StatusCode::OK);
}
