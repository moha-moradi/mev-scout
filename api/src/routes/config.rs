//! `GET/PUT /api/config` — sanitized config read + secret-safe edit.
//!
//! `GET` returns non-secret fields plus a masked RPC summary (hostnames only).
//! `PUT` merges non-secret fields into the **raw TOML value** on disk (never
//! the env-expanded in-memory `Config`, which would bake live API keys into
//! the file), validates via core `validation::validate_and_resolve`,
//! backs up to `{config}.bak.toml`, then writes. A changed `chain` (or
//! `output.db_path` / `explorer.db_path`) re-resolves both DB connections.
//! Edits are rejected (409) while a job is running.

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use mev_scout_core::config::settings::ExplorerConfig;
use mev_scout_core::config::validation;
use mev_scout_core::config::{BacktestConfig, Config, GasConfig, OutputConfig};

use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;

/// Masked RPC summary — provider count + hostnames only, never URLs/keys.
#[derive(Serialize)]
pub struct RpcSummary {
    pub providers: usize,
    pub hosts: Vec<String>,
}

/// Config DTO safe for API serialization — strips `rpc_urls`, `rpc_rps`,
/// `rpc_url`, `rps_limit`, `block_concurrency` and CLI-only fields.
#[derive(Serialize)]
pub struct SanitizedConfig {
    pub chain: String,
    pub gas: GasConfig,
    pub backtest: BacktestConfig,
    pub output: OutputConfig,
    pub explorer: ExplorerConfig,
    pub rpc: RpcSummary,
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/config", get(get_config).put(put_config))
}

fn rpc_summary(cfg: &Config) -> RpcSummary {
    let mut urls: Vec<&str> = cfg.rpc.rpc_urls.iter().map(String::as_str).collect();
    if let Some(u) = &cfg.rpc.rpc_url {
        urls.push(u);
    }
    let hosts = urls
        .iter()
        .filter_map(|u| url::Url::parse(u).ok())
        .filter_map(|u| match u.host_str() {
            Some(h) if !h.is_empty() => Some(h.to_string()),
            _ => None,
        })
        .collect();
    RpcSummary {
        providers: urls.len(),
        hosts,
    }
}

async fn get_config(State(state): State<SharedState>) -> ApiResult<Json<SanitizedConfig>> {
    let cfg = state.config.read().await;
    Ok(Json(SanitizedConfig {
        chain: cfg.chain.to_string(),
        gas: cfg.gas.clone(),
        backtest: cfg.backtest.clone(),
        output: cfg.output.clone(),
        explorer: cfg.explorer.clone(),
        rpc: rpc_summary(&cfg),
    }))
}

/// `PUT /api/config` body: a partial set of non-secret fields. Absent fields
/// keep their current on-disk values. Unknown fields are rejected.
#[derive(Deserialize)]
pub struct ConfigEdit {
    pub chain: Option<String>,
    pub gas: Option<GasConfig>,
    pub backtest: Option<BacktestConfig>,
    pub output: Option<OutputConfig>,
    pub explorer: Option<ExplorerConfig>,
}

#[derive(Serialize)]
pub struct ConfigEditResponse {
    pub ok: bool,
    pub restarted_connections: bool,
}

async fn put_config(
    State(state): State<SharedState>,
    Json(edit): Json<ConfigEdit>,
) -> ApiResult<Json<ConfigEditResponse>> {
    // Reject edits while a job runs: the child reads the same file, and a
    // mid-run chain switch would split the UI vs job views.
    if state.job_manager.lock().await.has_running().await {
        return Err(ApiError::conflict(
            "config edits are rejected while a job is running",
        ));
    }

    let path: PathBuf = state.config_path.clone();
    let raw = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| ApiError::bad_request(format!("cannot read config file: {e}")))?;
    let mut toml_value: toml::Value = raw
        .parse()
        .map_err(|e| ApiError::bad_request(format!("config file is not valid TOML: {e}")))?;

    let before_paths = {
        let cfg = state.config.read().await;
        (
            cfg.chain,
            cfg.output.db_path.clone(),
            cfg.explorer.db_path.clone(),
        )
    };

    // Merge each present section into the raw TOML. `gas`/`backtest`/
    // `output` are `#[serde(flatten)]`ed in `Config`, so their fields live
    // at the top level of the document (see `mev-scout.example.toml`) and
    // must be spliced flat. `explorer` is a named section → nested table.
    if let Some(chain) = &edit.chain {
        let parsed: mev_scout_core::types::ChainName = chain
            .parse()
            .map_err(|_| ApiError::bad_request(format!("unknown chain '{chain}'")))?;
        toml_value
            .as_table_mut()
            .ok_or_else(|| ApiError::bad_request("config root is not a table".to_string()))?
            .insert(
                "chain".to_string(),
                toml::Value::String(parsed.to_string()),
            );
    }
    if let Some(gas) = &edit.gas {
        merge_flattened(&mut toml_value, gas)?;
    }
    if let Some(backtest) = &edit.backtest {
        merge_flattened(&mut toml_value, backtest)?;
    }
    if let Some(output) = &edit.output {
        merge_flattened(&mut toml_value, output)?;
    }
    if let Some(explorer) = &edit.explorer {
        merge_section(&mut toml_value, "explorer", explorer)?;
    }

    // Validate the merged result with core validation before writing. The
    // editor gets the *full* bound checks (`validate_and_resolve_for`:
    // chain/rpc, gas_limit, rps_limit, proximity_window) — except strategy
    // checks, which `check_strategies=false` skips. Block range fields are
    // CLI-only (`#[serde(skip)]`, never in TOML), so inject a placeholder
    // range to satisfy the "exactly one range flag" invariant without
    // picking one for the user.
    let merged_toml = toml::to_string_pretty(&toml_value)
        .map_err(|e| ApiError::bad_request(format!("failed to serialize config: {e}")))?;
    let mut merged_cfg: Config = toml::from_str(&merged_toml)
        .map_err(|e| ApiError::bad_request(format!("merged config failed to parse: {e}")))?;
    merged_cfg.expand_env_secrets();
    merged_cfg.blocks = Some(1);
    if let Err(e) = validation::validate_and_resolve_for(&merged_cfg, false) {
        return Err(ApiError::bad_request(format!("invalid config: {e}")));
    }

    // Backup then write (single-level undo).
    let backup = backup_path(&path);
    tokio::fs::copy(&path, &backup)
        .await
        .map_err(|e| ApiError::internal(anyhow::anyhow!("backup failed: {e}")))?;
    tokio::fs::write(&path, &merged_toml)
        .await
        .map_err(|e| ApiError::internal(anyhow::anyhow!("write failed: {e}")))?;

    // Reload into state; swap DB connections when paths changed.
    let mut cfg = merged_cfg;
    cfg.config_path = Some(path.clone());
    *state.config.write().await = cfg;

    let after_paths = {
        let cfg = state.config.read().await;
        (
            cfg.chain,
            cfg.output.db_path.clone(),
            cfg.explorer.db_path.clone(),
        )
    };
    let paths_changed = before_paths != after_paths;
    state
        .resolve_connections(paths_changed)
        .await
        .map_err(ApiError::internal)?;

    Ok(Json(ConfigEditResponse {
        ok: true,
        restarted_connections: paths_changed,
    }))
}

fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mev-scout.toml".to_string());
    path.with_file_name(name + ".bak.toml")
}

/// Merge a typed `#[serde(flatten)]` section (already deserialized) into the
/// raw TOML by splicing its fields at the top level. `Config` flattens
/// `rpc`/`gas`/`backtest`/`output`, so nesting them under `[gas]` etc. would
/// produce TOML that fails to re-deserialize (unknown variant/field).
fn merge_flattened<T: Serialize>(
    toml_value: &mut toml::Value,
    section: &T,
) -> Result<(), ApiError> {
    let json: Value = serde_json::to_value(section)
        .map_err(|e| ApiError::bad_request(format!("section serialization failed: {e}")))?;
    if json.is_null() {
        return Ok(());
    }
    let obj = json.as_object().ok_or_else(|| {
        ApiError::bad_request("section must serialize to a table".to_string())
    })?;
    let Some(root) = toml_value.as_table_mut() else {
        return Err(ApiError::bad_request("config root is not a table".to_string()));
    };
    for (key, nested) in obj {
        let as_toml = json_to_toml(nested)
            .map_err(|e| ApiError::bad_request(format!("section conversion failed: {e}")))?;
        root.insert(key.clone(), as_toml);
    }
    Ok(())
}

/// Merge a typed named section (already deserialized) into the raw TOML under
/// `key` (e.g. `[explorer]`). `null` sections are dropped entirely (keep
/// on-disk values).
fn merge_section<T: Serialize>(
    toml_value: &mut toml::Value,
    key: &str,
    section: &T,
) -> Result<(), ApiError> {
    let json: Value = serde_json::to_value(section)
        .map_err(|e| ApiError::bad_request(format!("section serialization failed: {e}")))?;
    if json.is_null() {
        return Ok(());
    }
    let as_toml = json_to_toml(&json)
        .map_err(|e| ApiError::bad_request(format!("section conversion failed: {e}")))?;
    if let Some(root) = toml_value.as_table_mut() {
        root.insert(key.to_string(), as_toml);
    } else {
        return Err(ApiError::bad_request("config root is not a table".to_string()));
    }
    Ok(())
}

fn json_to_toml(json: &Value) -> anyhow::Result<toml::Value> {
    let serialized = serde_json::to_string(json)?;
    Ok(serde_json::from_str::<toml::Value>(&serialized)?)
}
