//! `GET/PUT /api/config` — config read + secret-aware edit.
//!
//! `GET` returns non-secret fields plus an RPC summary: masked hostnames and
//! the **raw on-disk** `rpc_urls` / `rpc_rps` (unexpanded `${ENV}` placeholders)
//! for the active chain (or `?chain=` preview target). It never returns
//! env-expanded in-memory URLs. Per-chain overrides under `[chains.<name>.rpc]`
//! win over the top-level (global default) keys.
//!
//! `PUT` merges fields into the **raw TOML value** on disk (never the
//! env-expanded in-memory `Config`, which would bake live API keys into the
//! file), validates via core `validation::validate_and_resolve`, backs up to
//! `{config}.bak.toml`, then writes and hot-reloads `AppState`. A changed
//! `chain` (or `output.db_path` / `explorer.db_path`) re-resolves both DB
//! connections. Edits are rejected (409) while a job is running.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::extract::Query;
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
/// the raw disk strings, not env-expanded memory. `rpc` is the active chain's
/// effective config; `per_chain_rpc` lists chains with `[chains.<name>.rpc]`
/// overrides on disk.
#[derive(Serialize)]
pub struct SanitizedConfig {
    pub chain: String,
    pub gas: GasConfig,
    pub backtest: BacktestConfig,
    pub output: OutputConfig,
    pub explorer: ExplorerConfig,
    pub rpc: RpcSummary,
    pub per_chain_rpc: HashMap<String, RpcSummary>,
}

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/config", get(get_config).put(put_config))
}

/// Read the raw TOML document from disk. Never env-expands.
fn raw_config_value(path: &Path) -> Option<toml::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.parse::<toml::Value>().ok()
}

/// Extract `rpc_urls` / `rpc_rps` (raw disk strings) from a single TOML table.
fn rpc_arrays_from_table(table: &toml::Table) -> (Vec<String>, Vec<f64>) {
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

/// Resolve the effective RPC arrays for a chain from the raw document: the
/// `[chains.<chain>.rpc]` override when present, else the top-level
/// (global default) keys.
fn rpc_arrays_for_chain(value: Option<&toml::Value>, chain: &str) -> (Vec<String>, Vec<f64>) {
    let Some(value) = value else {
        return (Vec::new(), Vec::new());
    };
    let Some(root) = value.as_table() else {
        return (Vec::new(), Vec::new());
    };
    let per_chain = root
        .get("chains")
        .and_then(|c| c.as_table())
        .and_then(|t| t.get(chain))
        .and_then(|c| c.as_table())
        .and_then(|t| t.get("rpc"))
        .and_then(|t| t.as_table());
    rpc_arrays_from_table(per_chain.unwrap_or(root))
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

fn rpc_summary_from_value(value: Option<&toml::Value>, chain: &str) -> RpcSummary {
    let (urls, rps) = rpc_arrays_for_chain(value, chain);
    let hosts = hosts_from_urls(&urls);
    RpcSummary {
        providers: urls.len(),
        hosts,
        urls,
        rps,
    }
}

/// Per-chain RPC overrides present on disk (`[chains.<name>.rpc]`), keyed by
/// chain name. Used by the Config UI to flag chains with custom RPC config.
fn per_chain_rpc_summaries(value: Option<&toml::Value>) -> HashMap<String, RpcSummary> {
    let mut out = HashMap::new();
    let Some(value) = value else {
        return out;
    };
    let Some(root) = value.as_table() else {
        return out;
    };
    let Some(chains) = root.get("chains").and_then(|c| c.as_table()) else {
        return out;
    };
    for (name, entry) in chains {
        let Some(rpc) = entry
            .as_table()
            .and_then(|t| t.get("rpc"))
            .and_then(|r| r.as_table())
        else {
            continue;
        };
        let (urls, rps) = rpc_arrays_from_table(rpc);
        if urls.is_empty() {
            continue;
        }
        let hosts = hosts_from_urls(&urls);
        out.insert(
            name.clone(),
            RpcSummary {
                providers: urls.len(),
                hosts,
                urls,
                rps,
            },
        );
    }
    out
}

async fn get_config(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<SanitizedConfig>> {
    let cfg = state.config.read().await;

    // `?chain=...` previews that chain's effective RPC for the Config UI
    // editor without switching the active chain. A typo earns a 400 rather
    // than a silent fallback to the active chain.
    let preview_chain = match params.get("chain") {
        Some(name) => Some(
            name.parse::<mev_scout_core::types::ChainName>()
                .map_err(|_| ApiError::bad_request(format!("unknown chain '{name}'")))?,
        ),
        None => None,
    };
    let chain = preview_chain.unwrap_or(cfg.chain).to_string();

    let raw = raw_config_value(&state.config_path);
    let rpc = rpc_summary_from_value(raw.as_ref(), &chain);
    let per_chain_rpc = per_chain_rpc_summaries(raw.as_ref());
    Ok(Json(SanitizedConfig {
        chain: cfg.chain.to_string(),
        gas: cfg.gas.clone(),
        backtest: cfg.backtest.clone(),
        output: cfg.output.clone(),
        explorer: cfg.explorer.clone(),
        rpc,
        per_chain_rpc,
    }))
}

/// `PUT /api/config` body: a partial set of fields. Absent fields keep their
/// current on-disk values. `rpc_urls`/`rpc_rps` write the top-level (global
/// default); `chain_rpc` writes per-chain `[chains.<name>.rpc]` overrides.
/// Unknown fields are rejected by serde deny if configured; otherwise ignored.
#[derive(Deserialize)]
pub struct ConfigEdit {
    pub chain: Option<String>,
    pub gas: Option<GasConfig>,
    pub backtest: Option<BacktestConfig>,
    pub output: Option<OutputConfig>,
    pub explorer: Option<ExplorerConfig>,
    pub rpc_urls: Option<Vec<String>>,
    pub rpc_rps: Option<Vec<f64>>,
    pub chain_rpc: Option<HashMap<String, ChainRpcEdit>>,
}

/// Per-chain RPC edits for `PUT /api/config`: `rpc_urls`/`rpc_rps` map to
/// `[chains.<name>.rpc]` on disk. Absent fields leave the on-disk values in
/// place.
#[derive(Deserialize)]
pub struct ChainRpcEdit {
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
    if let Some(chain_rpc) = &edit.chain_rpc {
        merge_per_chain_rpc(&mut toml_value, chain_rpc)?;
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
    // Config files commonly omit `[chains.*]` or only override RPC (built-in
    // defaults are merged at load time by `Config::load_or_default`); do the
    // same field-wise merge here so validation sees pool_discovery_start_block
    // / factories and does not fail with "no [chains.<chain>] section found".
    mev_scout_core::config::merge_default_chains(&mut merged_cfg.chains);
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

/// Merge per-chain RPC edits into `[chains.<name>.rpc]` of the raw TOML.
///
/// Pins each touched chain's real `chain_id` (`ChainConfig` requires it), so
/// a fresh override for a chain with no existing `[chains.<name>]` section
/// still re-parses. Only the edited keys are inserted — unrelated per-chain
/// fields are never touched.
fn merge_per_chain_rpc(
    toml_value: &mut toml::Value,
    edits: &HashMap<String, ChainRpcEdit>,
) -> Result<(), ApiError> {
    let root = toml_value
        .as_table_mut()
        .ok_or_else(|| ApiError::bad_request("config root is not a table".to_string()))?;
    let chains = root
        .entry("chains".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let Some(chains_table) = chains.as_table_mut() else {
        return Err(ApiError::bad_request(
            "config `chains` is not a table".to_string(),
        ));
    };

    for (name, edit) in edits {
        let parsed: mev_scout_core::types::ChainName = name
            .parse()
            .map_err(|_| ApiError::bad_request(format!("unknown chain '{name}'")))?;
        let entry = chains_table
            .entry(name.clone())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let Some(entry_table) = entry.as_table_mut() else {
            return Err(ApiError::bad_request(format!(
                "[chains.{name}] is not a table"
            )));
        };
        entry_table.insert(
            "chain_id".to_string(),
            toml::Value::Integer(parsed.chain_id() as i64),
        );
        let rpc_entry = entry_table
            .entry("rpc".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let Some(rpc_table) = rpc_entry.as_table_mut() else {
            return Err(ApiError::bad_request(format!(
                "[chains.{name}.rpc] is not a table"
            )));
        };
        if let Some(urls) = &edit.rpc_urls {
            rpc_table.insert(
                "rpc_urls".to_string(),
                toml::Value::Array(
                    urls.iter()
                        .map(|u| toml::Value::String(u.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(rps) = &edit.rpc_rps {
            rpc_table.insert(
                "rpc_rps".to_string(),
                toml::Value::Array(rps.iter().map(|n| toml::Value::Float(*n)).collect()),
            );
        }
    }
    Ok(())
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
