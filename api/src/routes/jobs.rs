//! Job endpoints: create (allowlisted commands), list, detail, log tail,
//! progress, stop.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::jobs::{JobInfo, ProgressEvent};
use crate::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/jobs", post(create_job).get(list_jobs))
        .route("/api/jobs/:id", get(job_detail))
        .route("/api/jobs/:id/log", get(job_log))
        .route("/api/jobs/:id/progress", get(job_progress))
        .route("/api/jobs/:id/stop", post(job_stop))
}

/// Commands the API may spawn (user decision: critical + auxiliary +
/// `explorer index`). Read-side `explorer` subcommands are served by API
/// read endpoints instead; `fetch`/`replay`/`config`/`validate-pools` stay
/// CLI-only.
pub const ALLOWED_COMMANDS: &[&str] = &[
    "run",
    "live",
    "discover",
    "tokens",
    "scan",
    "report",
    "explorer index",
];

#[derive(Deserialize)]
pub struct CreateJob {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Serialize)]
pub struct CreateJobResponse {
    pub job_id: String,
}

async fn create_job(
    State(state): State<SharedState>,
    Json(req): Json<CreateJob>,
) -> ApiResult<Json<CreateJobResponse>> {
    let cmd = req.command.trim();
    if !ALLOWED_COMMANDS.contains(&cmd) {
        return Err(ApiError::bad_request(format!(
            "command '{cmd}' is not allowlisted; allowed: {}",
            ALLOWED_COMMANDS.join(", ")
        )));
    }
    let job_id = state
        .job_manager
        .lock()
        .await
        .spawn(
            &state.config_path,
            cmd.to_string(),
            req.args,
            req.timeout_secs,
        )
        .await
        .map_err(|e| {
            if e.to_string().contains("already running") {
                ApiError::conflict(e.to_string())
            } else {
                ApiError::internal(e)
            }
        })?;
    Ok(Json(CreateJobResponse { job_id }))
}

async fn list_jobs(State(state): State<SharedState>) -> ApiResult<Json<Vec<JobInfo>>> {
    let jobs = state.job_manager.lock().await.list().await;
    Ok(Json(jobs))
}

async fn job_detail(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> ApiResult<Json<JobInfo>> {
    let info = state
        .job_manager
        .lock()
        .await
        .job_info(&id)
        .await
        .map_err(|e| {
            if e.to_string().contains("unknown job") {
                ApiError::not_found(e.to_string())
            } else {
                ApiError::internal(e)
            }
        })?;
    Ok(Json(info))
}

#[derive(Deserialize)]
pub struct LogQuery {
    pub tail: Option<usize>,
}

async fn job_log(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Json<Vec<String>>> {
    let lines = state
        .job_manager
        .lock()
        .await
        .log_tail(&id, q.tail.or(Some(100)))
        .await
        .map_err(|e| {
            if e.to_string().contains("unknown job") {
                ApiError::not_found(e.to_string())
            } else {
                ApiError::internal(e)
            }
        })?;
    Ok(Json(lines))
}

#[derive(Serialize)]
pub struct ProgressResponse {
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<f64>,
}

async fn job_progress(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Option<ProgressResponse>>> {
    let progress = state
        .job_manager
        .lock()
        .await
        .progress(&id)
        .await
        .map_err(|e| {
            if e.to_string().contains("unknown job") {
                ApiError::not_found(e.to_string())
            } else {
                ApiError::internal(e)
            }
        })?;
    let out = progress.map(|p: ProgressEvent| {
        let pct = p.pct();
        ProgressResponse {
            stage: p.stage,
            done: p.done,
            total: p.total,
            run_id: p.run_id,
            ops: p.ops,
            elapsed_ms: p.elapsed_ms,
            pct,
        }
    });
    Ok(Json(out))
}

async fn job_stop(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> ApiResult<Json<JobInfo>> {
    {
        let mut mgr = state.job_manager.lock().await;
        mgr.stop(&id).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unknown job") {
                ApiError::not_found(msg)
            } else if msg.contains("not running") {
                ApiError::conflict(msg)
            } else {
                ApiError::internal(e)
            }
        })?;
    }
    // Give the exit watcher a beat to resolve the final status.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let info = state
        .job_manager
        .lock()
        .await
        .job_info(&id)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(info))
}
