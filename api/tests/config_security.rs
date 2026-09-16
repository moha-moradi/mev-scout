//! Integration tests: `GET/PUT /api/config` — secret-safe edit round-trip,
//! `${ENV_VAR}` placeholder preservation, `.bak.toml` backup, 400 on invalid,
//! and the security requirement that `rpc_urls`/API keys never appear in any
//! config response (GET and PUT flows).

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{test_state, test_router, test_state_with_files, write_env_template_config, write_temp_config};

async fn request(
    app: &axum::Router<()>,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let resp = app
        .clone()
        .oneshot(match body {
            Some(v) => builder
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&v).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        })
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn config_get_returns_sanitized_non_secret_fields() {
    let state = test_state().await;
    let app = test_router(state);
    let (status, json) = request(&app, Method::GET, "/api/config", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["chain"], "polygon");
    assert!(json["gas"].is_object());
    assert!(json["backtest"].is_object());
    assert!(json["output"].is_object());
    assert!(json["explorer"].is_object());
    // test_state uses the default Config, whose rpc_urls list is empty.
    assert_eq!(json["rpc"]["providers"], 0);
    assert!(json["rpc"]["hosts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn security_rpc_urls_never_in_config_get() {
    // The harness config has `rpc_urls` set with a URL containing "SECRETKEY".
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
    let state = test_state_with_files(cfg_path).await;
    let app = test_router(state);

    let (status, json) = request(&app, Method::GET, "/api/config", None).await;
    assert_eq!(status, StatusCode::OK);
    let raw = serde_json::to_string(&json).unwrap();
    assert!(
        !raw.contains("rpc_url") && !raw.contains("rpc_rps") && !raw.contains("SECRETKEY"),
        "GET /api/config leaked secrets: {raw}"
    );
    assert!(!raw.contains("user:secret"), "credentials leaked: {raw}");
    // Masked summary: hostname only, never URL scheme/userinfo/path/key.
    assert_eq!(json["rpc"]["providers"], 1);
    assert!(
        !raw.contains("polygon-rpc.example.com/v1/SECRETKEY"),
        "full URL leaked: {raw}"
    );
}

#[tokio::test]
async fn security_rpc_urls_never_in_put_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
    let state = test_state_with_files(cfg_path).await;
    let app = test_router(state);

    // PUT a config edit — the response must not echo secrets either.
    let (status, resp) = request(
        &app,
        Method::PUT,
        "/api/config",
        Some(json!({
            "gas": { "gas_limit": 5000000 },
            "backtest": { "min_profit_wei": 2000000000 }
        })),
    )
    .await;
assert_eq!(status, StatusCode::OK, "PUT failed: {resp}");
    assert_eq!(resp["ok"], true);
    let raw = serde_json::to_string(&resp).unwrap();
    assert!(!raw.contains("SECRETKEY") && !raw.contains("rpc_urls"), "PUT response leaked: {raw}");

    // And the rerun of GET after the edit must still be clean.
    let (_, json) = request(&app, Method::GET, "/api/config", None).await;
    let raw = serde_json::to_string(&json).unwrap();
    assert!(
        !raw.contains("SECRETKEY") && !raw.contains("rpc_urls"),
        "GET after PUT leaked secrets: {raw}"
    );

    // The on-disk file must still contain the secrets verbatim.
    let on_disk = std::fs::read_to_string(dir.path().join("mev-scout.toml")).unwrap();
    assert!(on_disk.contains("SECRETKEY"), "secrets lost from disk");
    assert!(on_disk.contains("rpc_urls"));
}

#[tokio::test]
async fn config_put_updates_non_secret_fields_and_creates_backup() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
    let state = test_state_with_files(cfg_path.clone()).await;
    let app = test_router(state);

    let (status, resp) = request(
        &app,
        Method::PUT,
        "/api/config",
        Some(json!({
            "backtest": { "min_profit_wei": 7000000000_i64, "proximity_window": 5 }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["ok"], true);

    // Applied on disk.
    let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(on_disk.contains("min_profit_wei = 7000000000"));

    // Backup created.
    let bak = dir.path().join("mev-scout.toml.bak.toml");
    assert!(bak.exists(), ".bak.toml not created");
    let backup = std::fs::read_to_string(bak).unwrap();
    assert!(backup.contains("min_profit_wei = 1000000000"), "backup holds pre-edit value");

    // State reloaded.
    let (_, json) = request(&app, Method::GET, "/api/config", None).await;
    assert_eq!(json["backtest"]["min_profit_wei"], 7000000000_i64);
}

#[tokio::test]
async fn config_put_preserves_env_placeholders_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_env_template_config(dir.path());
    let state = test_state_with_files(cfg_path.clone()).await;
    let app = test_router(state);

    let (status, _) = request(
        &app,
        Method::PUT,
        "/api/config",
        Some(json!({ "chain": "arbitrum" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The raw file must keep the ${POLYGON_RPC_KEY} placeholder unchanged.
    let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(
        on_disk.contains("${POLYGON_RPC_KEY}"),
        "env placeholder got expanded/baked into disk: {on_disk}"
    );
}

#[tokio::test]
async fn config_put_rejects_unknown_chain_with_400() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
    let state = test_state_with_files(cfg_path).await;
    let app = test_router(state);

    let (status, json) = request(
        &app,
        Method::PUT,
        "/api/config",
        Some(json!({ "chain": "solana" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn config_put_rejects_invalid_value_with_400_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
    let state = test_state_with_files(cfg_path.clone()).await;
    let app = test_router(state);

    let before = std::fs::read_to_string(&cfg_path).unwrap();

    // gas_limit below the 21k floor — invalid per core validation.
    let (status, json) = request(
        &app,
        Method::PUT,
        "/api/config",
        Some(json!({ "gas": { "gas_limit": 500 } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(json["error"].is_string());

    // File untouched, no backup written.
    let after = std::fs::read_to_string(&cfg_path).unwrap();
    assert_eq!(before, after);
    assert!(!dir.path().join("mev-scout.toml.bak.toml").exists());
}

#[tokio::test]
async fn config_edit_rejected_409_while_job_runs() {
    // Build a state pointing at a real stub binary + temp config.
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
    let state = test_state_with_files(cfg_path).await;
    let app = test_router(state);

    // Start a long-running job via the jobs API (allowlisted `live` command;
    // the `emit-progress` arg makes the stub emit NDJSON then stay alive).
    let (status, resp) = request(
        &app,
        Method::POST,
        "/api/jobs",
        Some(json!({
            "command": "live",
            "args": ["emit-progress"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let job_id = resp["job_id"].as_str().unwrap().to_string();

    // Config edit while that job runs → 409.
    let (status, json) = request(
        &app,
        Method::PUT,
        "/api/config",
        Some(json!({ "chain": "bsc" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert!(json["error"].as_str().unwrap().contains("job is running"));

    // Clean up.
    request(&app, Method::POST, &format!("/api/jobs/{job_id}/stop"), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

#[tokio::test]
async fn config_chain_switch_swaps_connections() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
let state = test_state_with_files(cfg_path).await;
    let app = test_router(state.clone());

    let (status, _) = request(
        &app,
        Method::PUT,
        "/api/config",
        Some(json!({ "chain": "arbitrum" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, json) = request(&app, Method::GET, "/api/config", None).await;
    assert_eq!(json["chain"], "arbitrum");
    // Derived DB paths now point at arbitrum files.
    assert!(
        state
            .cache_db_path
            .read()
            .await
            .to_string_lossy()
            .contains("arbitrum")
    );
}
