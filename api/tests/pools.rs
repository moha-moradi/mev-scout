//! Integration tests: `/api/pools` filtering/sorting/pagination over the
//! file-backed cache DB (`SqliteStore::pools_filtered`).

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use common::{state_with_real_dbs, test_router};

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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn app() -> axum::Router<()> {
    test_router(state_with_real_dbs().await)
}

#[tokio::test]
async fn pools_lists_all() {
    let app = app().await;
    let (status, json) = get(&app, "/api/pools").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 3);
    assert_eq!(json["items"].as_array().unwrap().len(), 3);
    // Rich columns present.
    let first = &json["items"][0];
    assert!(first["dex_name"].is_string());
    assert!(first["token0_symbol"].is_string());
    assert!(first["token1_symbol"].is_string());
}

#[tokio::test]
async fn pools_limit_capped_at_1000() {
    let app = app().await;
    let (status, json) = get(&app, "/api/pools?limit=99999").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["limit"], 1000);
}

#[tokio::test]
async fn pools_dex_filter() {
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?dex=uniswap-v3").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["dex_name"], "uniswap-v3");
}

#[tokio::test]
async fn pools_token_filter() {
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?token=USDC").await;
    let items = json["items"].as_array().unwrap();
    // All three pools involve USDC as token0 or token1.
    assert_eq!(items.len(), 3);

    let (_, json) = get(&app, "/api/pools?token=WETH").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["token0_symbol"], "WETH");
}

#[tokio::test]
async fn pools_min_tvl_filter() {
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?min_tvl=400000").await;
    let items = json["items"].as_array().unwrap();
    // Only the 500k pool passes (300k < 400k, third has NULL tvl).
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["dex_name"], "uniswap-v3");
}

#[tokio::test]
async fn pools_sort_by_tvl_asc_desc() {
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?sort=tvl_usd&order=desc").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items[0]["dex_name"], "uniswap-v3"); // 500k
    assert_eq!(items[1]["dex_name"], "pancakeswap"); // 300k
                                                     // NULL tvl sorts last.
    assert_eq!(items[2]["dex_name"], "balancer");

    let (_, json) = get(&app, "/api/pools?sort=tvl_usd&order=asc").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items[0]["dex_name"], "pancakeswap");
}

#[tokio::test]
async fn pools_sort_by_volume_and_creation() {
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?sort=volume_usd_24h&order=desc").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items[0]["dex_name"], "uniswap-v3"); // 10000

    let (_, json) = get(&app, "/api/pools?sort=creation_block&order=desc").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items[0]["dex_name"], "balancer"); // block 300
}

#[tokio::test]
async fn pools_q_search() {
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?q=balancer").await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["dex_name"], "balancer");
}

#[tokio::test]
async fn pools_pagination() {
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?offset=1&limit=1").await;
    assert_eq!(json["total"], 3);
    assert_eq!(json["offset"], 1);
    assert_eq!(json["limit"], 1);
    assert_eq!(json["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn pools_null_tvl_handling() {
    // A pool with all-null TVL (balancer) sorts last on desc and is excluded
    // from min_tvl filters but still present in unfiltered listings.
    let app = app().await;
    let (_, json) = get(&app, "/api/pools?sort=tvl_usd&order=desc").await;
    let items = json["items"].as_array().unwrap();
    let balancer = items.iter().find(|p| p["dex_name"] == "balancer").unwrap();
    assert!(balancer["tvl_usd"].is_null());
    // The API response still includes it so the UI can detect all-null TVL
    // and offer the "Enrich pools" action.
    assert!(json["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["tvl_usd"].is_null()));
}
