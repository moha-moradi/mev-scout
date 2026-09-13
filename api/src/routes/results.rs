//! Results endpoints: list runs w/ aggregates, run detail, validation, PnL.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use mev_scout_core::cache::RunManifest;
use mev_scout_core::explorer::store::MevOpportunity;
use mev_scout_core::explorer::validate::ValidationReport;

use crate::error::{ApiError, ApiResult};
use crate::pagination::{paginate, Paginated};
use crate::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/results", get(list_results))
        .route("/api/results/:run_id", get(result_detail))
        .route("/api/results/:run_id/validation", get(result_validation))
        .route("/api/results/:run_id/pnl", get(result_pnl))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

/// One row of `/api/results`: manifest + opportunity aggregates.
#[derive(Serialize)]
pub struct ResultsRow {
    pub run_id: String,
    pub chain: String,
    pub start_block: u64,
    pub end_block: u64,
    pub range_mode: String,
    pub strategies: Vec<String>,
    pub resolved_at: u64,
    pub total_ops: u64,
    pub total_net_profit_usd: Option<f64>,
}

async fn list_results(
    State(state): State<SharedState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Paginated<ResultsRow>>> {
    let limit = q.limit.unwrap_or(50).min(100);
    let offset = q.offset.unwrap_or(0);

    let manifests = {
        let conn = state.cache_conn.lock().await;
        query_manifests(&conn)?
    };
    // Aggregate per-run opportunity counts (and USD where recorded) from
    // the explorer store, keyed by run_id.
    let summaries = opportunity_totals(&state).await?;

    let rows: Vec<ResultsRow> = manifests
        .into_iter()
        .map(|m| {
            let (total_ops, net_usd) = summaries.get(&m.run_id).cloned().unwrap_or((0, None));
            ResultsRow {
                run_id: m.run_id,
                chain: m.chain,
                start_block: m.start_block,
                end_block: m.end_block,
                range_mode: m.range_mode,
                strategies: m.strategies,
                resolved_at: m.resolved_at,
                total_ops,
                total_net_profit_usd: net_usd,
            }
        })
        .collect();
    Ok(Json(paginate(rows, offset, limit)))
}

/// manifest query against the raw read-only cache connection.
pub fn query_manifests(conn: &rusqlite::Connection) -> anyhow::Result<Vec<RunManifest>> {
    let mut stmt = conn.prepare(
        "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
         FROM run_manifests
         ORDER BY resolved_at DESC, rowid DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(RunManifest {
            run_id: r.get(0)?,
            chain: r.get(1)?,
            start_block: r.get::<_, i64>(2)? as u64,
            end_block: r.get::<_, i64>(3)? as u64,
            resolved_at: r.get::<_, i64>(4)? as u64,
            range_mode: r.get(5)?,
            strategies: r
                .get::<_, String>(6)?
                .split(',')
                .map(str::to_string)
                .collect(),
            flash_loan_provider: r.get(7)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// run_id → (opportunity count, Σ profit_usd) from the explorer `opportunities` table.
/// Profit USD is best-effort: rows carry raw token amounts, so this counts
/// rows with any expected_profit > 0 and sums nothing USD-wise unless the
/// (future) USD column exists. Kept as count + None for MVP honesty.
async fn opportunity_totals(
    state: &SharedState,
) -> anyhow::Result<std::collections::HashMap<String, (u64, Option<f64>)>> {
    let conn = state.explorer_conn.lock().await;
    let mut stmt =
        conn.prepare("SELECT run_id, COUNT(*) FROM opportunities GROUP BY run_id")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
    })?;
    let mut out = std::collections::HashMap::new();
    for r in rows {
        let (run_id, count) = r?;
        out.insert(run_id, (count, None));
    }
    Ok(out)
}

/// Full run detail: manifest header + reconstructed opportunities.
#[derive(Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub manifest: RunManifest,
    pub opportunities: Vec<MevOpportunity>,
}

async fn result_detail(
    State(state): State<SharedState>,
    Path(run_id): Path<String>,
) -> ApiResult<Json<RunDetail>> {
    let manifest = {
        let conn = state.cache_conn.lock().await;
        query_manifest(&conn, &run_id)?
    };
    let manifest = manifest.ok_or_else(|| ApiError::not_found(format!("run '{run_id}'")))?;
    let opportunities = crate::read::opportunities_by_run(&state, &run_id).await?;
    Ok(Json(RunDetail {
        manifest,
        opportunities,
    }))
}

fn query_manifest(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> anyhow::Result<Option<RunManifest>> {
    let mut stmt = conn.prepare(
        "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
         FROM run_manifests WHERE run_id = ?1",
    )?;
    let mut rows = stmt.query_map([run_id], |r| {
        Ok(RunManifest {
            run_id: r.get(0)?,
            chain: r.get(1)?,
            start_block: r.get::<_, i64>(2)? as u64,
            end_block: r.get::<_, i64>(3)? as u64,
            resolved_at: r.get::<_, i64>(4)? as u64,
            range_mode: r.get(5)?,
            strategies: r
                .get::<_, String>(6)?
                .split(',')
                .map(str::to_string)
                .collect(),
            flash_loan_provider: r.get(7)?,
        })
    })?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Validation cross-check for a run: tiered recall vs realized explorer ops
/// + coverage info telling the UI whether to offer "Index this range".
#[derive(Serialize)]
pub struct ValidationResponse {
    #[serde(flatten)]
    pub report: ValidationReport,
    pub explorer_coverage: CoverageInfo,
}

#[derive(Serialize)]
pub struct CoverageInfo {
    pub blocks_indexed: u64,
    pub blocks_total: u64,
    pub covered: bool,
}

async fn result_validation(
    State(state): State<SharedState>,
    Path(run_id): Path<String>,
) -> ApiResult<Json<ValidationResponse>> {
    let manifest = {
        let conn = state.cache_conn.lock().await;
        query_manifest(&conn, &run_id)?
    };
    let manifest = manifest.ok_or_else(|| ApiError::not_found(format!("run '{run_id}'")))?;
    let chain = state.active_chain().await;

    // Coverage: how many blocks in [start, end] does the explorer know?
    let (blocks_indexed, blocks_total) = {
        let conn = state.explorer_conn.lock().await;
        let total = (manifest.end_block - manifest.start_block + 1) as u64;
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM blocks WHERE block_number BETWEEN ?1 AND ?2",
                rusqlite::params![manifest.start_block as i64, manifest.end_block as i64],
                |r| r.get(0),
            )
            .unwrap_or(0);
        (indexed as u64, total)
    };
    let covered = blocks_total > 0 && blocks_indexed == blocks_total;

    // compute_validation needs an ExplorerStore handle; open a temporary
    // read handle over the same file (it opens read-write, which is fine
    // locally and matches the CLI's usage).
    let explorer_path = state.explorer_db_path.read().await.clone();
    let store = mev_scout_core::explorer::store::ExplorerStore::open(&explorer_path)
        .map_err(ApiError::internal)?;
    let report = mev_scout_core::explorer::validate::compute_validation(
        &store,
        chain,
        manifest.start_block,
        manifest.end_block,
        0,
        Some(&[run_id.clone()]),
        false,
    )
    .map_err(ApiError::internal)?;

    Ok(Json(ValidationResponse {
        report,
        explorer_coverage: CoverageInfo {
            blocks_indexed,
            blocks_total,
            covered,
        },
    }))
}

/// Simulated session PnL: Σ expected_profit per profit token − gas, with
/// USD legs best-effort via the explorer `prices` table.
#[derive(Serialize)]
pub struct PnlResponse {
    pub simulated: bool,
    pub per_token: Vec<TokenPnl>,
    pub totals: PnlTotals,
}

#[derive(Serialize, Default)]
pub struct TokenPnl {
    pub token: String,
    pub gross: String,
    pub gas: String,
    pub net: String,
    pub usd: Option<f64>,
}

#[derive(Serialize, Default)]
pub struct PnlTotals {
    pub gross_usd: Option<f64>,
    pub gas_usd: Option<f64>,
    pub net_usd: Option<f64>,
}

async fn result_pnl(
    State(state): State<SharedState>,
    Path(run_id): Path<String>,
) -> ApiResult<Json<PnlResponse>> {
    let ops = crate::read::opportunities_by_run(&state, &run_id).await?;
    // Aggregate per token_out (profit token assumed = token_out).
    use std::collections::BTreeMap;
    let mut per_token: BTreeMap<String, (f64, f64)> = BTreeMap::new(); // (gross, gas) float-wei
    for op in &ops {
        let token = format!("{:#x}", op.token_out);
        let e = per_token.entry(token).or_insert((0.0, 0.0));
        e.0 += op.expected_profit.to_string().parse::<f64>().unwrap_or(0.0);
        e.1 += op.gas_cost_wei as f64;
    }

    // USD conversion via prices table, best-effort per token.
    let conn = state.explorer_conn.lock().await;
    let hour = mev_scout_core::utils::epoch_secs() / 3600;
    let mut rows = Vec::new();
    let mut gross_usd = 0.0;
    let mut gas_usd = 0.0;
    let mut usd_known = false;
    for (token, (gross, gas)) in per_token {
        let usd = conn
            .query_row(
                "SELECT usd FROM prices WHERE token = ?1 AND hour <= ?2 AND hour > ?2 - 24
                 ORDER BY hour DESC LIMIT 1",
                rusqlite::params![token, hour as i64],
                |r| r.get::<_, f64>(0),
            )
            .ok();
        let gross_val = if usd.is_some() {
            gross_usd += gross * usd.unwrap();
            usd_known = true;
            Some(gross * usd.unwrap())
        } else {
            None
        };
        gas_usd += gas; // native wei; USD conversion needs native price — omitted when unknown
        rows.push(TokenPnl {
            token,
            gross: format_number(gross),
            gas: format_number(gas),
            net: format_number(gross - gas),
            usd: gross_val,
        });
    }
    drop(conn);

    Ok(Json(PnlResponse {
        simulated: true,
        per_token: rows,
        totals: PnlTotals {
            gross_usd: if usd_known { Some(gross_usd) } else { None },
            gas_usd: None, // native-wei gas USD omitted unless price known
            net_usd: None,
        },
    }))
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}
