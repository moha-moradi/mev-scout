//! `GET /api/health` — lightweight liveness + DB status probe.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::error::ApiResult;
use crate::state::{check_db_status, DbStatus, SharedState};

#[derive(Serialize)]
pub struct HealthResponse {
    pub version: &'static str,
    pub chain: String,
    pub db_status: DbStatusDto,
    pub rpc_provider_count: usize,
    pub job_status: JobStatusDto,
    pub uptime_seconds: u64,
}

#[derive(Serialize)]
pub struct DbStatusDto {
    pub cache: &'static str,
    pub explorer: &'static str,
}

#[derive(Serialize)]
pub struct JobStatusDto {
    pub running: Option<String>,
    pub total: usize,
}

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/health", get(health))
}

async fn health(State(state): State<SharedState>) -> ApiResult<Json<HealthResponse>> {
    let cfg = state.config.read().await;
    let cache_path = state.cache_db_path.read().await.clone();
    let explorer_path = state.explorer_db_path.read().await.clone();
    let cache_status = check_db_status(&cache_path, "run_manifests");
    let explorer_status = check_db_status(&explorer_path, "mev_ops");
    let eff_rpc = cfg.effective_rpc(cfg.chain);
    let rpc_provider_count = eff_rpc.rpc_urls.len() + usize::from(eff_rpc.rpc_url.is_some());
    drop(cfg);

    let jobs = state.job_manager.lock().await.list().await;
    let running = jobs
        .iter()
        .find(|j| j.status == crate::jobs::JobStatus::Running)
        .map(|j| j.job_id.clone());

    Ok(Json(HealthResponse {
        version: state.version,
        chain: state.active_chain().await.to_string(),
        db_status: DbStatusDto {
            cache: cache_status.as_str(),
            explorer: explorer_status.as_str(),
        },
        rpc_provider_count,
        job_status: JobStatusDto {
            running,
            total: jobs.len(),
        },
        uptime_seconds: state.started_at.elapsed().as_secs(),
    }))
}

/// Silence unused import warning when DbStatus is re-exported via state.
#[allow(unused_imports)]
use DbStatus as _DbStatusGuard;
