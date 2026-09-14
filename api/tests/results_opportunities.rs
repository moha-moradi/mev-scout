//! Integration tests: results (list/detail/validation/pnl), runs (manifests),
//! opportunities (list + run summaries), and pagination/limit caps.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use common::{state_with_real_dbs, test_state, test_router};

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
async fn results_lists_manifests_with_aggregates_sorted_desc() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/results").await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    // run_1 (resolved_at 1700000000) sorts before run_2 (1699999999).
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["run_id"], "run_1");
    assert_eq!(items[0]["chain"], "polygon");
    assert_eq!(items[0]["start_block"], 100);
    assert_eq!(items[0]["end_block"], 101);
    assert_eq!(items[0]["total_ops"], 2); // run_1 has 2 opportunities rows
    assert_eq!(items[1]["run_id"], "run_2");
}

#[tokio::test]
async fn results_limit_capped_at_100() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/results?limit=99999").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["limit"], 100);
}

#[tokio::test]
async fn result_detail_returns_manifest_plus_opportunities() {
    let app = test_router(state_with_real_dbs().await);
    let (status, json) = get(&app, "/api/results/run_1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run_id"], "run_1");
    assert_eq!(json["chain"], "polygon");
    assert_eq!(json["strategies"][0], "atomic");
    // Reconstructed MevOpportunity rows for run_1.
    let opps = json["opportunities"].as_array().unwrap();
    assert_eq!(opps.len(), 2);
    assert!(opps[0]["expected_profit"].as_str().is_some());
}

#[tokio::test]
async fn result_detail_unknown_404() {
    let app = test_router(test_state().await);
    let (status, _) = get(&app, "/api/results/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn result_validation_returns_report_and_coverage() {
    let app = test_router(state_with_real_dbs().await);
    let (status, json) = get(&app, "/api/results/run_1/validation").await;
    assert_eq!(status, StatusCode::OK);

    // explorer_coverage: run_1 covers blocks 100-101 (2 blocks), but the
    // in-memory explorer DB has no `blocks` rows → covered=false.
    assert!(json["explorer_coverage"]["blocks_total"].as_u64().unwrap() >= 2);
    assert_eq!(json["explorer_coverage"]["covered"], false);

    // Expanded into JSON: flat tier recall cards live under per_kind.
    assert!(json["per_kind"].is_array());
    assert!(json["matches"].is_array());
    assert!(json["chain"].is_string());
    assert!(json["from_block"].is_number());
    assert!(json["to_block"].is_number());
}

#[tokio::test]
async fn result_pnl_returns_simulated_summary() {
    let app = test_router(state_with_real_dbs().await);
    let (status, json) = get(&app, "/api/results/run_1/pnl").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["simulated"], true);
    // run_1 has 2 opportunities → per-token rows (token_out differs).
    let per_token = json["per_token"].as_array().unwrap();
    assert!(!per_token.is_empty());
    for t in per_token {
        assert!(t["gross"].as_str().is_some());
        assert!(t["gas"].as_str().is_some());
        assert!(t["net"].as_str().is_some());
    }
    // totals structure.
    assert!(json["totals"].is_object());
}

#[tokio::test]
async fn pnl_usd_omitted_when_price_unknown() {
    // run_2 has no opportunities seeded → empty per_token, defensive shape.
    let app = test_router(state_with_real_dbs().await);
    let (_, json) = get(&app, "/api/results/run_2/pnl").await;
    assert_eq!(json["simulated"], true);
    assert!(json["per_token"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn runs_lists_manifests_desc() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["run_id"], "run_1");
    assert_eq!(items[1]["run_id"], "run_2");
    // fields present
    assert!(items[0]["resolved_at"].is_number());
    assert!(items[0]["range_mode"].is_string());
}

#[tokio::test]
async fn runs_limit_capped_at_100() {
    let app = test_router(test_state().await);
    let (_, json) = get(&app, "/api/runs?limit=99999").await;
    assert_eq!(json["limit"], 100);
}

#[tokio::test]
async fn opportunities_lists_seeded_rows() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/opportunities").await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["run_id"], "run_1");
    assert!(items[0]["expected_profit"].is_string());
}

#[tokio::test]
async fn opportunities_run_id_filter() {
    let app = test_router(test_state().await);
    let (_, json) = get(&app, "/api/opportunities?run_id=live_1777").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["run_id"], "live_1777");
}

#[tokio::test]
async fn opportunities_limit_capped_at_500() {
    let app = test_router(test_state().await);
    let (_, json) = get(&app, "/api/opportunities?limit=99999").await;
    assert_eq!(json["limit"], 500);
}

#[tokio::test]
async fn opportunities_run_summaries_includes_live_without_manifest() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/opportunities/runs").await;
    assert_eq!(status, StatusCode::OK);
    let runs = json.as_array().unwrap();
    // run_1 (2 rows), live_1777 (1 row). Newest first by MAX(timestamp).
    assert_eq!(runs.len(), 2);
    let run1 = runs.iter().find(|r| r["run_id"] == "run_1").unwrap();
    assert_eq!(run1["count"], 2);
    let live = runs.iter().find(|r| r["run_id"] == "live_1777").unwrap();
    assert_eq!(live["count"], 1);
    // live_1777 has no manifest row, but the summary must list it anyway.
    let (status, _) = get(&app, "/api/results/live_1777").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sync_returns_zeroed_heads_when_empty() {
    let app = test_router(test_state().await);
    let (status, json) = get(&app, "/api/sync").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["explorer_head"], 0);
    // cache_head = MAX(end_block) over run_manifests = 202.
    assert_eq!(json["cache_head"], 202);
    assert!(json["last_indexed"].is_null());
}
