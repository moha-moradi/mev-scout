//! Wiremock coverage for the receipts batch RPC used by pool discovery.
//!
//! The fixtures are a real Avalanche C-Chain block and its receipts. They are
//! served from one HTTP POST per window so the tests exercise the same batch
//! shape (`eth_getBlockByNumber` + `eth_getBlockReceipts` per block) as the
//! discovery job.

use mev_scout_core::pool::discovery::{
    discover_pools_from_receipts, DiscoveryConfig, ReceiptScanOptions,
};
use mev_scout_core::rpc::RpcClient;
use serde_json::{json, Value};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const BLOCK: &str = include_str!("fixtures/receipts-block.json");
const RECEIPTS: &str = include_str!("fixtures/receipts-receipts.json");

fn fixture_block() -> Value {
    serde_json::from_str(BLOCK).expect("valid block fixture")
}

fn fixture_receipts() -> Value {
    serde_json::from_str(RECEIPTS).expect("valid receipts fixture")
}

/// Reply to every JSON-RPC call in a batch with the fixture matching its method,
/// preserving the client's request ids (alloy matches responses by id).
fn batch_responder(block: Value, receipts: Value) -> impl Fn(&Request) -> ResponseTemplate {
    move |request: &Request| {
        let calls: Vec<Value> = serde_json::from_slice(&request.body).expect("JSON-RPC array");
        let out: Vec<Value> = calls
            .into_iter()
            .map(|call| {
                let result = if call["method"] == "eth_getBlockByNumber" {
                    // The fixtures are one real block; patch its number so a
                    // multi-block batch decodes to the requested heights.
                    let mut block = block.clone();
                    block["number"] = call["params"][0].clone();
                    block
                } else {
                    receipts.clone()
                };
                json!({
                    "jsonrpc": "2.0",
                    "id": call["id"],
                    "result": result,
                })
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(out)
    }
}

async fn mock_server_with_fixtures() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(batch_responder(fixture_block(), fixture_receipts()))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn multi_block_receipts_batch_decodes_one_http_post() {
    let server = mock_server_with_fixtures().await;
    let rpc = RpcClient::from_urls(&[&server.uri()], 43114).expect("rpc client");

    let bundles = rpc
        .get_blocks_and_receipts_batch(&[101_330_359, 101_330_360])
        .await
        .expect("batch succeeded");

    assert_eq!(bundles.len(), 2);
    assert_eq!(bundles[0].0.number, 101_330_359);
    assert_eq!(bundles[1].0.number, 101_330_360);
    assert_eq!(bundles[0].2.len(), 2, "receipts were decoded");
    // One batch POST, not two sequential round-trips.
    assert_eq!(
        server.received_requests().await.expect("request log").len(),
        1
    );
    assert_eq!(rpc.metrics_snapshot().requests, 1);
}

#[tokio::test]
async fn pinned_multi_block_batch_uses_the_selected_provider() {
    let first = mock_server_with_fixtures().await;
    let second = mock_server_with_fixtures().await;
    let rpc = RpcClient::from_urls(&[&first.uri(), &second.uri()], 43114).expect("rpc client");

    let bundles = rpc
        .get_blocks_and_receipts_batch_for_many(1, &[101_330_359, 101_330_360])
        .await
        .expect("pinned batch succeeded");

    assert_eq!(bundles.len(), 2);
    assert_eq!(
        second.received_requests().await.expect("request log").len(),
        1
    );
    assert!(
        first.received_requests().await.expect("request log").is_empty(),
        "pinned provider 0 must not receive traffic"
    );
}

fn empty_config<'a>() -> DiscoveryConfig<'a> {
    DiscoveryConfig {
        batch_size: 1000,
        factory_batch_size: 1000,
        v2_fee_override: None,
        balancer_vault: None,
        v2_factories: None,
        v3_factories: None,
        curve_registry: None,
        curve_factories: None,
        solidly_factories: None,
        camelot_factories: None,
        v4_pool_manager: None,
        infinity_cl_pool_manager: None,
        metric_factory: None,
        fluid_factory: None,
        solidly_fee_bps: None,
        trader_joe_factories: None,
        pendle_factory: None,
        rpc_concurrency: 1,
        token_cache: None,
        pool_cache: None,
    }
}

#[tokio::test]
async fn null_receipts_mark_the_window_failed_and_skip_the_watermark() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(batch_responder(fixture_block(), Value::Null))
        .mount(&server)
        .await;
    let rpc = RpcClient::from_urls(&[&server.uri()], 43114).expect("rpc client");
    let cache = mev_scout_core::cache::SqliteStore::open(":memory:").expect("cache");

    let result = discover_pools_from_receipts(
        &rpc,
        &cache,
        101_330_359,
        101_330_360,
        &empty_config(),
        ReceiptScanOptions {
            batch_blocks: 2,
            cache_blocks: false,
            provider_idx: Some(0),
            mixed_recent: None,
            concurrency: 1,
            on_window: None,
        },
    )
    .await
    .expect("scan result");

    assert!(result.pools.is_empty());
    assert_eq!(result.failed_windows, vec![(101_330_359, 101_330_360)]);
    // callers must not advance the watermark when `failed_windows` is non-empty.
    let metrics = rpc.metrics_snapshot();
    assert!(metrics.requests > 0);
    assert!(metrics.errors > 0);
}
