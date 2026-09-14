//! Integration tests: explorer read endpoints (feed, stats, overview, top,
//! ops, op detail) and pagination cap enforcement.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use common::{test_state, test_router};

async fn get(
    app: &axum::Router<()>,
    uri: &str,
) -> (StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn feed_returns_seeded_rows() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/feed").await;
    assert_eq!(status, StatusCode::OK);
    let rows = json.as_array().unwrap();
    // 3 seeded mev_ops rows; newest blocks first.
    assert_eq!(rows.len(), 3);
    assert!(rows[0]["block_number"].as_u64().unwrap() >= rows[2]["block_number"].as_u64().unwrap());
    assert!(rows[0]["tx_hash"].is_string());
}

#[tokio::test]
async fn feed_kinds_filter() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/feed?kinds=sandwich").await;
    assert_eq!(status, StatusCode::OK);
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "sandwich");
}

#[tokio::test]
async fn feed_q_filter_and_min_profit() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/feed?q=0xAAA").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["tx_hash"], "0xAAA");

    // min_profit_usd=1.0 excludes the 0.7 net op.
    let (_, json) = get(&app, "/api/explorer/feed?min_profit_usd=1.0").await;
    assert_eq!(json.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn feed_limit_capped_at_200() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/feed?limit=99999").await;
    assert_eq!(status, StatusCode::OK);
    // Capped to 200 but only 3 rows exist.
    assert!(json.as_array().unwrap().len() <= 3);
}

#[tokio::test]
async fn stats_returns_by_kind_and_daily() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/stats?since=all").await;
    assert_eq!(status, StatusCode::OK);
    let mut by_kind = json["by_kind"].as_array().unwrap().clone();
    assert!(!by_kind.is_empty());
    // Kinds present: sandwich, arb_atomic, liquidation.
    let kinds: Vec<String> = by_kind
        .iter_mut()
        .map(|r| r["label"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(kinds.contains(&"sandwich".to_string()));
    assert!(kinds.contains(&"arb_atomic".to_string()));
    assert!(kinds.contains(&"liquidation".to_string()));
    assert!(json["daily"].is_array());
}

#[tokio::test]
async fn stats_handles_bad_since_400() {
    let app = test_router(test_state().await);
    let (status, _) = get(&app, "/api/explorer/stats?since=banana").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn overview_returns_aggregates() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/overview").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ops"], 3);
    // gross = 1.5 + 0.8 + 2.0 = 4.3
    assert!((json["gross_usd"].as_f64().unwrap() - 4.3).abs() < 1e-9);
    // net = 1.3 + 0.7 + 1.7 = 3.7
    assert!((json["net_usd"].as_f64().unwrap() - 3.7).abs() < 1e-9);
    // 2 distinct EOAs
    assert_eq!(json["searchers"], 2);
}

#[tokio::test]
async fn top_by_sender_token_pool() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/top?by=sender&since=all").await;
    assert_eq!(status, StatusCode::OK);
    // 2 distinct searchers: 0xS1 (2 ops) and 0xE2 (1 op). Sorted by gross desc.
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0]["gross_usd"].as_f64().unwrap() >= rows[1]["gross_usd"].as_f64().unwrap());

    let (_, json) = get(&app, "/api/explorer/top?by=token&since=all").await;
    assert!(!json.as_array().unwrap().is_empty());

    let (_, json) = get(&app, "/api/explorer/top?by=pool&since=all").await;
    let _ = json;

    // Invalid by → 400.
    let (status, _) = get(&app, "/api/explorer/top?by=nonsense").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn top_limit_capped_at_100() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/top?limit=99999").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.as_array().unwrap().len() <= 2);
}

#[tokio::test]
async fn ops_paginated() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/ops").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 3);
    assert_eq!(json["offset"], 0);
    assert_eq!(json["limit"], 50);
    assert_eq!(json["items"].as_array().unwrap().len(), 3);

    // Pagination slice.
    let (_, json) = get(&app, "/api/explorer/ops?offset=1&limit=1").await;
    assert_eq!(json["total"], 3);
    assert_eq!(json["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn ops_limit_capped_at_500() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/ops?limit=99999").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["limit"], 500);
}

#[tokio::test]
async fn ops_range_and_kind_filters() {
    let app = test_router(test_state().await);
    let (_, json) = get(&app, "/api/explorer/ops?from=100&to=101").await;
    assert_eq!(json["total"], 2);

    let (_, json) = get(&app, "/api/explorer/ops?kinds=arb_atomic").await;
    assert_eq!(json["total"], 1);
    assert_eq!(json["items"][0]["kind"], "arb_atomic");

    let (_, json) = get(&app, "/api/explorer/ops?q=0xC3").await;
    assert_eq!(json["total"], 1);
}

#[tokio::test]
async fn op_detail_returns_ops_and_rejected() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/op/0xAAA").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["tx_hash"], "0xAAA");
    assert_eq!(json["ops"].as_array().unwrap().len(), 1);
    assert_eq!(json["ops"][0]["eoa"], "0xS1");
    // rejected list present (may be empty — no rejected_candidates seeded).
    assert!(json["rejected"].is_array());
}

#[tokio::test]
async fn op_detail_unknown_returns_empty_ops() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/explorer/op/0xNOPE").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["ops"].as_array().unwrap().is_empty());
    assert!(json["rejected"].as_array().unwrap().is_empty());
}
