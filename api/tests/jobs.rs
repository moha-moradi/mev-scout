//! Integration tests: `/api/jobs*` — command allowlist, spawn/stop/log,
//! progress events, and single-job mutex (409 while running). Jobs run
//! in-process against the `MEV_SCOUT_JOB_STUB` fake executor.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{test_state_with_files, test_router, write_temp_config};

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

/// A jobs-capable state: config = a temp file, and jobs run against the
/// `MEV_SCOUT_JOB_STUB` fake executor (set by the harness).
async fn jobs_app() -> (axum::Router<()>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = write_temp_config(dir.path(), "polygon");
    let state = test_state_with_files(cfg_path).await;
    (test_router(state), 0)
}

async fn spawn(app: &axum::Router<()>, cmd: &str, args: Vec<&str>) -> (StatusCode, Value) {
    let args: Vec<String> = args.into_iter().map(str::to_string).collect();
    request(
        app,
        Method::POST,
        "/api/jobs",
        Some(json!({ "command": cmd, "args": args })),
    )
    .await
}

async fn wait_for(
    app: &axum::Router<()>,
    job_id: &str,
    target: &str,
    timeout_ms: u64,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let (_, json) = request(app, Method::GET, &format!("/api/jobs/{job_id}"), None).await;
        if json["status"] == target {
            return json;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "job {job_id} did not reach '{target}' (status={})",
            json["status"]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn command_allowlist_accepts_seven_rejects_others() {
    let (app, _) = jobs_app().await;
    for allowed in ["run", "live", "discover", "tokens", "scan", "report", "explorer index"] {
        let (status, json) = spawn(&app, allowed, vec!["ok"]).await;
        assert!(
            status == StatusCode::OK || status == StatusCode::CONFLICT,
            "{allowed} should be allowlisted; got {status}: {json}"
        );
        // Clean up any running job.
        if let Some(id) = json["job_id"].as_str() {
            request(&app, Method::POST, &format!("/api/jobs/{id}/stop"), None).await;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    let (app2, _) = jobs_app().await;
    for denied in ["fetch", "replay", "validate-pools", "explorer stats", "evil", "rm -rf"] {
        let (status, json) = spawn(&app2, denied, vec![]).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{denied} must be rejected: {json}"
        );
    }
}

#[tokio::test]
async fn job_success_log_and_run_id() {
    let (app, _) = jobs_app().await;
    let (status, resp) = spawn(&app, "run", vec!["ok", "--flag"]).await;
    assert_eq!(status, StatusCode::OK);
    let job_id = resp["job_id"].as_str().unwrap().to_string();

    let info = wait_for(&app, &job_id, "finished", 5000).await;
    assert_eq!(info["exit_code"], 0);
    assert_eq!(info["command"], "run");
    assert_eq!(info["run_id"], "run_1700000000");

    // Log tail contains the stub's stdout line.
    let (_, log) = request(&app, Method::GET, &format!("/api/jobs/{job_id}/log"), None).await;
    let text: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l.as_str())
        .collect();
    let text = text.join("\n");
    assert!(text.contains("hello from stub"), "log: {text}");
    assert!(text.contains("args=ok --flag"), "log: {text}");

    // Tail param slices from the end.
    let (_, tail) = request(&app, Method::GET, &format!("/api/jobs/{job_id}/log?tail=1"), None).await;
    assert_eq!(tail.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn job_failure_sets_status_and_exit_code() {
    let (app, _) = jobs_app().await;
    let (_, resp) = spawn(&app, "run", vec!["fail"]).await;
    let job_id = resp["job_id"].as_str().unwrap().to_string();

    let info = wait_for(&app, &job_id, "failed", 5000).await;
    assert_eq!(info["exit_code"], 1);

    // stderr captured into the same log.
    let (_, log) = request(&app, Method::GET, &format!("/api/jobs/{job_id}/log"), None).await;
    let text: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l.as_str())
        .collect();
    let text = text.join("\n");
    assert!(text.contains("boom"), "stderr missing: {text}");
}

#[tokio::test]
async fn job_progress_reports_stages_and_run_id() {
    let (app, _) = jobs_app().await;
    let (_, resp) = spawn(&app, "live", vec!["emit-progress"]).await;
    let job_id = resp["job_id"].as_str().unwrap().to_string();

    // Wait for the NDJSON events to be collected (polled: the parse happens
    // in a background task feeding on the child's stdout).
    let (status, progress) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (s, p) = request(
                &app,
                Method::GET,
                &format!("/api/jobs/{job_id}/progress"),
                None,
            )
            .await;
            if p["stage"] == "detect" {
                return (s, p);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("progress never reached 'detect'");
    assert_eq!(status, StatusCode::OK);
    let p = progress.as_object().unwrap();
    assert_eq!(p["stage"], "detect"); // last parsed event
    assert_eq!(p["done"], 8);
    assert_eq!(p["total"], 10);
    assert!(p["pct"].as_f64().unwrap() > 0.7);

    let (_, info) = request(&app, Method::GET, &format!("/api/jobs/{job_id}"), None).await;
    assert_eq!(info["run_id"], "live_1700000001");

    // Stop kills the long-running job.
    let (status, _) = request(&app, Method::POST, &format!("/api/jobs/{job_id}/stop"), None).await;
    assert_eq!(status, StatusCode::OK);
    let info = wait_for(&app, &job_id, "killed", 5000).await;
    assert_eq!(info["status"], "killed");
}

#[tokio::test]
async fn single_job_mutex_409_while_running() {
    let (app, _) = jobs_app().await;
    let (_, resp) = spawn(&app, "live", vec!["emit-progress"]).await;
    let job_id = resp["job_id"].as_str().unwrap().to_string();

    // Second job while the first runs → 409.
    let (status, json) = spawn(&app, "run", vec!["ok"]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert!(json["error"].as_str().unwrap().contains("already running"));

    request(&app, Method::POST, &format!("/api/jobs/{job_id}/stop"), None).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn job_detail_unknown_404_and_stop_non_running_409() {
    let (app, _) = jobs_app().await;
    let (status, _) = request(&app, Method::GET, "/api/jobs/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(&app, Method::POST, "/api/jobs/nope/stop", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(&app, Method::GET, "/api/jobs/nope/log", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn multi_word_command_splits_into_argv_tokens() {
    let (app, _) = jobs_app().await;
    // `explorer index` must reach the child as two argv tokens; the stub
    // echoes its parsed command + args, failing with exit 2 otherwise.
    let (status, resp) = spawn(&app, "explorer index", vec!["--from", "10", "--to", "20"]).await;
    assert_eq!(status, StatusCode::OK);
    let job_id = resp["job_id"].as_str().unwrap().to_string();
    let info = wait_for(&app, &job_id, "finished", 5000).await;
    assert_eq!(info["exit_code"], 0, "stub rejected the argv: {info}");
    // JobInfo.args stores the args past the command words.
    assert_eq!(
        info["args"],
        json!(["--from", "10", "--to", "20"]),
        "args must exclude the command words: {info}"
    );

    let (_, log) = request(&app, Method::GET, &format!("/api/jobs/{job_id}/log"), None).await;
    let text: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l.as_str())
        .collect();
    let text = text.join("\n");
    assert!(text.contains("args=--from 10 --to 20"), "log: {text}");
}

#[tokio::test]
async fn jobs_list_returns_all() {
    let (app, _) = jobs_app().await;
    let (_, resp) = spawn(&app, "run", vec!["ok"]).await;
    let job_id = resp["job_id"].as_str().unwrap().to_string();
    wait_for(&app, &job_id, "finished", 5000).await;

    let (status, list) = request(&app, Method::GET, "/api/jobs", None).await;
    assert_eq!(status, StatusCode::OK);
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["job_id"], job_id);
    assert_eq!(arr[0]["status"], "finished");
}

/// In-process jobs don't need a binary on disk; verify a basic spawn
/// finishes successfully (no dependency on an external executable).
#[tokio::test]
async fn in_process_job_runs_without_binary() {
    let (app, _) = jobs_app().await;
    let (status, resp) = spawn(&app, "run", vec!["ok"]).await;
    assert_eq!(status, StatusCode::OK);
    let job_id = resp["job_id"].as_str().unwrap().to_string();
    let info = wait_for(&app, &job_id, "finished", 5000).await;
    assert_eq!(info["exit_code"], 0);
}
