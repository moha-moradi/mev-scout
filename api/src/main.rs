//! mev-scout API server — local-only web UI + API layer over the SQLite
//! stores and the `mev-scout` CLI binary.

mod error;
mod jobs;
mod pagination;
mod read;
mod routes;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use clap::Parser;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use mev_scout_core::config::Config;

use state::{AppState, SharedState};

#[derive(Parser, Debug)]
#[command(name = "mev-scout-api", version, about = "mev-scout web API server")]
struct Args {
    /// TCP port to bind (local-only: 127.0.0.1)
    #[arg(long, default_value = "7600")]
    port: u16,

    /// Path to the mev-scout TOML config file
    #[arg(long, default_value = "mev-scout.toml")]
    config: PathBuf,

    /// Directory for job logs and API data
    #[arg(long, default_value = "api_data")]
    data_dir: PathBuf,

    /// Explicit path to the mev-scout CLI binary (default: sibling of this
    /// executable)
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Serve the built frontend from this directory (default: ./web/dist)
    #[arg(long, default_value = "web/dist")]
    web_dir: PathBuf,

    /// Allow cross-origin requests from the Vite dev server
    #[arg(long, default_value = "http://localhost:5173")]
    cors_origin: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    // Resolve CLI binary path: --binary override, else sibling of this exe.
    let binary_path = match &args.binary {
        Some(p) => p.clone(),
        None => {
            let exe = std::env::current_exe()?;
            let parent = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let exe_name = if cfg!(windows) { "mev-scout.exe" } else { "mev-scout" };
            parent.join(exe_name)
        }
    };
    if !binary_path.exists() {
        tracing::warn!(
            "mev-scout binary not found at {}; job spawning will fail until built",
            binary_path.display()
        );
    }

    // Load config (falls back to defaults when the file does not exist).
    let config = Config::load_or_default(&args.config.to_string_lossy())?;
    let cache_path = PathBuf::from(
        config.effective_db_path(&config.chain),
    );
    let explorer_path = PathBuf::from(config.effective_explorer_db_path(&config.chain));

    std::fs::create_dir_all(&args.data_dir)?;

    let state: SharedState = Arc::new(AppState {
        config: tokio::sync::RwLock::new(config),
        config_path: args.config.clone(),
        binary_path,
        data_dir: args.data_dir.clone(),
        explorer_db_path: tokio::sync::RwLock::new(explorer_path.clone()),
        cache_db_path: tokio::sync::RwLock::new(cache_path.clone()),
        explorer_conn: tokio::sync::Mutex::new(
            state::open_read_only_or_empty(&explorer_path)?,
        ),
        cache_conn: tokio::sync::Mutex::new(state::open_readonly_if_exists(&cache_path)?),
        job_manager: Arc::new(tokio::sync::Mutex::new(jobs::JobManager::new(
            &args.data_dir,
        ))),
        started_at: std::time::Instant::now(),
        version: env!("CARGO_PKG_VERSION"),
    });

    // Static frontend: /assets/* (hashed, immutable) + SPA fallback.
    let web_dir = args.web_dir.clone();
    let index_file = web_dir.join("index.html");
    let serve_assets = ServeDir::new(&web_dir).not_found_service(ServeFile::new(&index_file));

    let app = Router::new()
        .merge(routes::api_router())
        .fallback_service(serve_assets)
        .layer(if cfg!(debug_assertions) {
            CorsLayer::very_permissive()
        } else {
            CorsLayer::new()
        })
        .route("/api", get(api_index))
        .with_state(state.clone());

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
    tracing::info!("mev-scout-api listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state))
        .await?;
    Ok(())
}

async fn shutdown_signal(state: SharedState) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown: killing running child jobs");
    state.job_manager.lock().await.kill_all().await;
}

async fn api_index() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "name": "mev-scout-api",
            "endpoints": ["/api/health", "/api/chains", "/api/config", "/api/explorer/*",
                          "/api/results/*", "/api/runs", "/api/opportunities*", "/api/pools",
                          "/api/sync", "/api/jobs*"]
        })),
    )
}
