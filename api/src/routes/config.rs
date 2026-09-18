//! `GET/PUT /api/config` — config read + secret-aware edit.
//!
//! `GET` returns non-secret fields plus an RPC summary: masked hostnames and
//! the **raw on-disk** `rpc_urls` / `rpc_rps` (unexpanded `${ENV}` placeholders).
//! It never returns env-expanded in-memory URLs.
//!
//! `PUT` merges fields into the **raw TOML value** on disk (never the
//! env-expanded in-memory `Config`, which would bake live API keys into the
//! file), validates via core `validation::validate_and_resolve`, backs up to
//! `{config}.bak.toml`, then writes and hot-reloads `AppState`. A changed
//! `chain` (or `output.db_path` / `explorer.db_path`) re-resolves both DB
//! connections. Edits are rejected (409) while a job is running.

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

/// RPC summary for the Config UI: masked hosts plus raw disk URLs/RPS.
#[derive(Serialize)]
pub struct RpcSummary {
    pub providers: usize,
    pub hosts: Vec<String>,
    /// Unexpanded `rpc_urls` from the TOML file (may contain `${ENV}`).
    pub urls: Vec<String>,
    /// On-disk `rpc_rps` (aligned with `urls` when set).
    pub rps: Vec<f64>,
}

/// Config DTO for API serialization — strips CLI-only fields; RPC URLs are
/// the raw disk strings, not env-expanded memory.
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
    Router::new().route("/api/config", get(get_config).put(put_config))
}

/// Parse raw `rpc_urls` / `rpc_rps` from the config file without env expansion.
fn read_raw_rpc(path: &Path) -> (Vec<String>, Vec<f64>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(value) = raw.parse::<toml::Value>() else {
        return (Vec::new(), Vec::new());
    };
    let Some(table) = value.as_table() else {
        return (Vec::new(), Vec::new());
    };

    let mut urls = Vec::new();
    if let Some(arr) = table.get("rpc_urls").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                urls.push(s.to_string());
            }
        }
    }
    // Legacy single URL — include for display if present and not already listed.
    if let Some(s) = table.get("rpc_url").and_then(|v| v.as_str()) {
        if !urls.iter().any(|u| u == s) {
            urls.push(s.to_string());
        }
    }

    let mut rps = Vec::new();
    if let Some(arr) = table.get("rpc_rps").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(n) = item.as_float() {
                rps.push(n);
            } else if let Some(n) = item.as_integer() {
                rps.push(n as f64);
            }
        }
    }

    (urls, rps)
}

fn hosts_from_urls(urls: &[String]) -> Vec<String> {
    urls.iter()
        .filter_map(|u| url::Url::parse(u).ok())
        .filter_map(|u| match u.host_str() {
            Some(h) if !h.is_empty() => Some(h.to_string()),
            _ => None,
        })
        .collect()
}

fn rpc_summary_from_disk(path: &Path) -> RpcSummary {
    let (urls, rps) = read_raw_rpc(path);
    let hosts = hosts_from_urls(&urls);
    RpcSummary {
        providers: urls.len(),
        hosts,
        urls,
        rps,
    }
}

async fn get_config(State(state): State<SharedState>) -> ApiResult<Json<SanitizedConfig>> {
    let cfg = state.config.read().await;
    let rpc = rpc_summary_from_disk(&state.config_path);
    Ok(Json(SanitizedConfig {
        chain: cfg.chain.to_string(),
        gas: cfg.gas.clone(),
        backtest: cfg.backtest.clone(),
        output: cfg.output.clone(),
        explorer: cfg.explorer.clone(),
        rpc,
    }))
}

/// `PUT /api/config` body: a partial set of fields. Absent fields keep their
/// current on-disk values. Unknown fields are rejected by serde deny if
/// configured; otherwise ignored.
#[derive(Deserialize)]
pub struct ConfigEdit {
    pub chain: Option<String>,
    pub gas: Option<GasConfig>,
    pub backtest: Option<BacktestConfig>,
    pub output: Option<OutputConfig>,
    pub explorer: Option<ExplorerConfig>,
    pub rpc_urls: Option<Vec<String>>,
    pub rpc_rps: Option<Vec<f64>>,
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
    // `output`/`rpc_*` are `#[serde(flatten)]`ed in `Config`, so their fields
    // live at the top level of the document (see `mev-scout.example.toml`) and
    // must be spliced flat. `explorer` is a named section → nested table.
    if let Some(chain) = &edit.chain {
        let parsed: mev_scout_core::types::ChainName = chain
            .parse()
            .map_err(|_| ApiError::bad_request(format!("unknown chain '{chain}'")))?;
        toml_value
            .as_table_mut()
            .ok_or_else(|| ApiError::bad_request("config root is not a table".to_string()))?
            .insert("chain".to_string(), toml::Value::String(parsed.to_string()));
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
    if let Some(urls) = &edit.rpc_urls {
        let root = toml_value
            .as_table_mut()
            .ok_or_else(|| ApiError::bad_request("config root is not a table".to_string()))?;
        let arr = toml::Value::Array(
            urls.iter()
                .map(|u| toml::Value::String(u.clone()))
                .collect(),
        );
        root.insert("rpc_urls".to_string(), arr);
        // Prefer the list form; drop legacy single URL so it cannot shadow.
        root.remove("rpc_url");
    }
    if let Some(rps) = &edit.rpc_rps {
        let root = toml_value
            .as_table_mut()
            .ok_or_else(|| ApiError::bad_request("config root is not a table".to_string()))?;
        let arr = toml::Value::Array(rps.iter().map(|n| toml::Value::Float(*n)).collect());
        root.insert("rpc_rps".to_string(), arr);
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
    // Config files commonly omit `[chains.*]` (built-in defaults are merged
    // at load time by `Config::load_or_default`); do the same here so
    // validation sees the resolved chain section instead of failing with
    // "no [chains.<chain>] section found".
    for (name, default_cfg) in mev_scout_core::config::default_chains() {
        merged_cfg.chains.entry(name).or_insert(default_cfg);
    }
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
    let obj = json
        .as_object()
        .ok_or_else(|| ApiError::bad_request("section must serialize to a table".to_string()))?;
    let Some(root) = toml_value.as_table_mut() else {
        return Err(ApiError::bad_request(
            "config root is not a table".to_string(),
        ));
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
        return Err(ApiError::bad_request(
            "config root is not a table".to_string(),
        ));
    }
    Ok(())
}

fn json_to_toml(json: &Value) -> anyhow::Result<toml::Value> {
    let serialized = serde_json::to_string(json)?;
    Ok(serde_json::from_str::<toml::Value>(&serialized)?)
}
