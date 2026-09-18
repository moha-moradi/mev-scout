//! Explorer read endpoints (feed, stats, overview, top, ops, op/:tx_hash).

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use mev_scout_core::explorer::store::{FeedRow, MevOpRow, OverviewRow, RejectedRow, StatsRow};
use mev_scout_core::explorer::types::MevKind;

use crate::error::{ApiError, ApiResult};
use crate::pagination::{paginate, Paginated};
use crate::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/explorer/feed", get(feed))
        .route("/api/explorer/stats", get(stats))
        .route("/api/explorer/overview", get(overview))
        .route("/api/explorer/top", get(top))
        .route("/api/explorer/ops", get(ops))
        .route("/api/explorer/op/:tx_hash", get(op_detail))
        .route("/api/explorer/doctor", get(doctor))
        .route("/api/explorer/export", get(export_download))
}

#[derive(Deserialize)]
pub struct FeedQuery {
    pub limit: Option<u64>,
    pub kinds: Option<String>,
    pub q: Option<String>,
    pub min_profit_usd: Option<f64>,
}

/// Live feed tail. `q` filters by address/hash; `min_profit_usd` filters
/// rows in the API layer (parity with the CLI live-feed flag).
async fn feed(
    State(state): State<SharedState>,
    Query(q): Query<FeedQuery>,
) -> ApiResult<Json<Vec<FeedRow>>> {
    state
        .ensure_explorer_conn()
        .await
        .map_err(ApiError::internal)?;
    let limit = q.limit.unwrap_or(50).min(200) as usize;
    let kinds = parse_kinds(q.kinds.as_deref())?;
    let chain = state.active_chain().await;
    let rows = {
        let conn = state.explorer_conn.lock().await;
        query_feed_tail(&conn, limit, &kinds)?
    };
    // Ensure Price column can render even before the indexer caches a quote.
    let native = match rows.first().and_then(|r| r.native_price_usd) {
        Some(v) => Some(v),
        None => mev_scout_core::explorer::pricing::fetch_native_price_coingecko(chain)
            .await
            .ok(),
    };
    let min = q.min_profit_usd.unwrap_or(0.0);
    let out: Vec<FeedRow> = rows
        .into_iter()
        .filter(|r| r.net_profit_usd.unwrap_or(r.profit_usd.unwrap_or(0.0)) >= min)
        .filter(|r| match &q.q {
            Some(needle) => feed_row_matches(r, needle),
            None => true,
        })
        .map(|mut r| {
            if r.native_price_usd.is_none() {
                r.native_price_usd = native;
            }
            r
        })
        .collect();
    Ok(Json(out))
}

fn feed_row_matches(r: &FeedRow, needle: &str) -> bool {
    let n = needle.to_ascii_lowercase();
    r.tx_hash.to_lowercase().contains(&n)
        || r.eoa.to_lowercase().contains(&n)
        || r.kind.to_lowercase().contains(&n)
        || r.profit_token
            .as_deref()
            .map(|t| t.to_lowercase().contains(&n))
            .unwrap_or(false)
}

/// feed_tail without going through ExplorerStore (we hold a raw read-only
/// connection in AppState).
fn query_feed_tail(
    conn: &rusqlite::Connection,
    limit: usize,
    kinds: &[MevKind],
) -> anyhow::Result<Vec<FeedRow>> {
    let order = if kinds.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
        format!("WHERE kind IN ({})", list.join(","))
    };
    let sql = format!(
        "SELECT ts, block_number, kind, profit_token, profit_amount, profit_usd, gas_cost_usd,
                net_profit_usd, eoa, tx_hash, route_json
         FROM mev_ops {order}
         ORDER BY block_number DESC, tx_index DESC, id DESC LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_feed)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    let native = latest_native_price(conn)?;
    for row in &mut out {
        row.native_price_usd = native;
    }
    Ok(out)
}

fn map_feed(r: &rusqlite::Row<'_>) -> rusqlite::Result<FeedRow> {
    Ok(FeedRow {
        ts: r.get::<_, i64>(0)? as u64,
        block_number: r.get::<_, i64>(1)? as u64,
        kind: r.get(2)?,
        profit_token: r.get(3)?,
        profit_amount: r.get(4)?,
        profit_usd: r.get(5)?,
        gas_cost_usd: r.get(6)?,
        net_profit_usd: r.get(7)?,
        eoa: r.get(8)?,
        tx_hash: r.get(9)?,
        route_json: r.get(10)?,
        native_price_usd: None,
    })
}

fn latest_native_price(conn: &rusqlite::Connection) -> anyhow::Result<Option<f64>> {
    let mut stmt = conn.prepare(
        "SELECT usd FROM prices
         WHERE lower(token) IN ('0x0000000000000000000000000000000000000000', '0x0')
         ORDER BY hour DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, f64>(0)?))
    } else {
        Ok(None)
    }
}

fn parse_kinds(s: Option<&str>) -> ApiResult<Vec<MevKind>> {
    match s {
        None => Ok(vec![]),
        Some("") => Ok(vec![]),
        Some(list) => {
            let mut out = Vec::new();
            for part in list.split(',') {
                let k = part.trim();
                if k.is_empty() {
                    continue;
                }
                let kind = MevKind::parse(k)
                    .ok_or_else(|| ApiError::bad_request(format!("unknown kind '{k}'")))?;
                out.push(kind);
            }
            Ok(out)
        }
    }
}

/// `since` param: `1d`, `7d`, `30d`, `all` (or raw unix seconds).
pub fn parse_since(s: Option<&str>) -> ApiResult<u64> {
    let now = mev_scout_core::utils::epoch_secs();
    match s {
        None | Some("all") | Some("") => Ok(0),
        Some("1d") => Ok(now.saturating_sub(86_400)),
        Some("7d") => Ok(now.saturating_sub(7 * 86_400)),
        Some("30d") => Ok(now.saturating_sub(30 * 86_400)),
        Some(raw) => raw
            .parse()
            .map_err(|_| ApiError::bad_request(format!("invalid since '{raw}'"))),
    }
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub by_kind: Vec<StatsRow>,
    pub daily: Vec<StatsRow>,
}

#[derive(Deserialize)]
pub struct StatsQuery {
    pub since: Option<String>,
}

async fn stats(
    State(state): State<SharedState>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Json<StatsResponse>> {
    state
        .ensure_explorer_conn()
        .await
        .map_err(ApiError::internal)?;
    let since = parse_since(q.since.as_deref())?;
    let conn = state.explorer_conn.lock().await;
    let by_kind = grouped_stats(&conn, "kind", since)?;
    let daily = daily_stats(&conn, since)?;
    Ok(Json(StatsResponse { by_kind, daily }))
}

fn grouped_stats(
    conn: &rusqlite::Connection,
    group_col: &str,
    since_ts: u64,
) -> anyhow::Result<Vec<StatsRow>> {
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts}
         GROUP BY label
         ORDER BY gross_usd DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn daily_stats(conn: &rusqlite::Connection, since_ts: u64) -> anyhow::Result<Vec<StatsRow>> {
    let sql = format!(
        "SELECT date(ts, 'unixepoch') AS label,
                COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts}
         GROUP BY label
         ORDER BY label DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn map_stats(r: &rusqlite::Row<'_>) -> rusqlite::Result<StatsRow> {
    Ok(StatsRow {
        label: r.get(0)?,
        ops: r.get(1)?,
        gross_usd: r.get(2)?,
        net_usd: r.get(3)?,
    })
}

#[derive(Deserialize)]
pub struct OverviewQuery {
    pub since: Option<String>,
}

async fn overview(
    State(state): State<SharedState>,
    Query(q): Query<OverviewQuery>,
) -> ApiResult<Json<OverviewRow>> {
    state
        .ensure_explorer_conn()
        .await
        .map_err(ApiError::internal)?;
    let since = parse_since(q.since.as_deref())?;
    let conn = state.explorer_conn.lock().await;
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(profit_usd), 0),
                COALESCE(SUM(net_profit_usd), 0),
                COALESCE(SUM(gas_cost_usd), 0),
                COALESCE(MAX(profit_usd), 0),
                COUNT(DISTINCT eoa)
         FROM mev_ops WHERE ts >= {since}"
    );
    let row = conn
        .query_row(&sql, [], |r| {
            Ok(OverviewRow {
                ops: r.get::<_, i64>(0)?,
                gross_usd: r.get(1)?,
                net_usd: r.get(2)?,
                gas_usd: r.get(3)?,
                highest_single_usd: r.get(4)?,
                searchers: r.get::<_, i64>(5)?,
            })
        })
        .map_err(|e| ApiError::internal(anyhow::anyhow!("overview query failed: {e}")))?;
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct TopQuery {
    pub by: Option<String>,
    pub since: Option<String>,
    pub limit: Option<u64>,
    pub q: Option<String>,
}

async fn top(
    State(state): State<SharedState>,
    Query(q): Query<TopQuery>,
) -> ApiResult<Json<Vec<StatsRow>>> {
    state
        .ensure_explorer_conn()
        .await
        .map_err(ApiError::internal)?;
    let since = parse_since(q.since.as_deref())?;
    let limit = q.limit.unwrap_or(20).min(100) as usize;
    let conn = state.explorer_conn.lock().await;
    let mut rows = match q.by.as_deref() {
        None | Some("sender") => grouped_top(&conn, "eoa", since, limit)?,
        Some("token") => grouped_top(&conn, "profit_token", since, limit)?,
        Some("pool") => top_pools(&conn, since, limit)?,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "invalid 'by' value '{other}' (sender|token|pool)"
            )))
        }
    };
    if let Some(needle) = &q.q {
        let n = needle.to_ascii_lowercase();
        rows.retain(|r| r.label.to_lowercase().contains(&n));
    }
    Ok(Json(rows))
}

fn grouped_top(
    conn: &rusqlite::Connection,
    group_col: &str,
    since_ts: u64,
    limit: usize,
) -> anyhow::Result<Vec<StatsRow>> {
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts} AND {group_col} IS NOT NULL
         GROUP BY label
         ORDER BY gross_usd DESC
         LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn top_pools(
    conn: &rusqlite::Connection,
    since_ts: u64,
    limit: usize,
) -> anyhow::Result<Vec<StatsRow>> {
    let sql = format!(
        "SELECT j.value->>'$.pool' AS label,
                COUNT(DISTINCT m.id) AS ops,
                COALESCE(SUM(m.profit_usd), 0) AS gross_usd,
                COALESCE(SUM(m.net_profit_usd), 0) AS net_usd
         FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
         WHERE m.ts >= {since_ts} AND j.value->>'$.pool' IS NOT NULL
         GROUP BY label
         ORDER BY gross_usd DESC
         LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[derive(Deserialize)]
pub struct OpsQuery {
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub kinds: Option<String>,
    pub q: Option<String>,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

async fn ops(
    State(state): State<SharedState>,
    Query(q): Query<OpsQuery>,
) -> ApiResult<Json<Paginated<MevOpRow>>> {
    state
        .ensure_explorer_conn()
        .await
        .map_err(ApiError::internal)?;
    let from = q.from.unwrap_or(0);
    let to = q.to.unwrap_or(u64::MAX / 4);
    let kinds = parse_kinds(q.kinds.as_deref())?;
    let limit = q.limit.unwrap_or(50).min(500);
    let offset = q.offset.unwrap_or(0);

    let conn = state.explorer_conn.lock().await;
    let kind_filter = if kinds.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
        format!("AND kind IN ({})", list.join(","))
    };
    let sql = format!(
        "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                confidence, canonical_id, profit_token, profit_amount, profit_usd,
                gas_cost_usd, net_profit_usd, route_json, victim_hashes,
                details_json, detector, created_at
         FROM mev_ops
         WHERE block_number BETWEEN {from} AND {to} {kind_filter}
         ORDER BY block_number, tx_index"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_op)?;
    let mut all = Vec::new();
    for r in rows {
        let row = r?;
        if let Some(needle) = &q.q {
            let n = needle.to_ascii_lowercase();
            let matches = row.tx_hash.to_lowercase().contains(&n)
                || row.eoa.to_lowercase().contains(&n)
                || row
                    .contract
                    .as_deref()
                    .map(|c| c.to_lowercase().contains(&n))
                    .unwrap_or(false)
                || row.kind.to_lowercase().contains(&n)
                || row
                    .profit_token
                    .as_deref()
                    .map(|t| t.to_lowercase().contains(&n))
                    .unwrap_or(false);
            if !matches {
                continue;
            }
        }
        all.push(row);
    }
    drop(stmt);
    drop(conn);
    Ok(Json(paginate(all, offset, limit)))
}

fn map_op(r: &rusqlite::Row<'_>) -> rusqlite::Result<MevOpRow> {
    Ok(MevOpRow {
        id: r.get(0)?,
        block_number: r.get::<_, i64>(1)? as u64,
        tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        tx_hash: r.get(3)?,
        ts: r.get::<_, i64>(4)? as u64,
        kind: r.get(5)?,
        eoa: r.get(6)?,
        contract: r.get(7)?,
        confidence: r.get(8)?,
        canonical_id: r.get(9)?,
        profit_token: r.get(10)?,
        profit_amount: r.get(11)?,
        profit_usd: r.get(12)?,
        gas_cost_usd: r.get(13)?,
        net_profit_usd: r.get(14)?,
        route_json: r.get(15)?,
        victim_hashes: r.get(16)?,
        details_json: r.get(17)?,
        detector: r.get(18)?,
        created_at: r.get::<_, i64>(19)? as u64,
    })
}

/// Combined op-detail response: realized ops + rejected candidates in the
/// surrounding block window (±10 blocks for rejection context).
#[derive(Serialize)]
pub struct ExplainResponse {
    pub tx_hash: String,
    pub ops: Vec<MevOpRow>,
    pub rejected: Vec<RejectedRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

#[derive(Deserialize)]
pub struct OpDetailQuery {
    /// When true, run debug_traceTransaction (prestateTracer) and attach summary.
    pub trace: Option<bool>,
}

async fn op_detail(
    State(state): State<SharedState>,
    Path(tx_hash): Path<String>,
    Query(q): Query<OpDetailQuery>,
) -> ApiResult<Json<ExplainResponse>> {
    state
        .ensure_explorer_conn()
        .await
        .map_err(ApiError::internal)?;
    let chain = state.active_chain().await;
    let want_trace = q.trace.unwrap_or(false);

    // Keep the rusqlite MutexGuard inside this block so it cannot cross awaits.
    let (ops, rejected) = {
        let conn = state.explorer_conn.lock().await;
        let sql = "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                          confidence, canonical_id, profit_token, profit_amount, profit_usd,
                          gas_cost_usd, net_profit_usd, route_json, victim_hashes,
                          details_json, detector, created_at
                   FROM mev_ops WHERE tx_hash = ?1 ORDER BY id";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params![tx_hash], map_op)?;
        let mut ops = Vec::new();
        for r in rows {
            ops.push(r?);
        }
        drop(stmt);

        let mut rejected = Vec::new();
        if let Some(first) = ops.first() {
            let from = first.block_number.saturating_sub(10);
            let to = first.block_number.saturating_add(10);
            let rsql =
                "SELECT block_number, tx_index, strategy, pool_a, pool_b, token_in, token_out,
                               expected_profit, gas_cost_wei, reject_reason, detail
                        FROM rejected_candidates
                        WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3
                        ORDER BY block_number, tx_index";
            let mut rstmt = conn.prepare(rsql)?;
            let rows = rstmt.query_map(
                rusqlite::params![chain.to_string(), from as i64, to as i64],
                map_rejected,
            )?;
            for r in rows {
                rejected.push(r?);
            }
        }
        (ops, rejected)
    };

    let mut trace = None;
    if want_trace {
        let config = state.config.read().await.clone();
        let tx_hash_clone = tx_hash.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(anyhow::anyhow!("runtime: {e}")));
                    return;
                }
            };
            let result = rt.block_on(mev_scout_core::jobs::job_trace_op(
                &config,
                &tx_hash_clone,
                &mev_scout_core::progress::NoopProgress,
            ));
            let _ = tx.send(result);
        });
        let outcome = rx
            .await
            .map_err(|_| ApiError::internal(anyhow::anyhow!("trace worker dropped")))?
            .map_err(ApiError::internal)?;
        trace = Some(outcome.summary);
    }

    Ok(Json(ExplainResponse {
        tx_hash,
        ops,
        rejected,
        trace,
    }))
}

async fn doctor(
    State(state): State<SharedState>,
) -> ApiResult<Json<mev_scout_core::jobs::DoctorOutcome>> {
    let config = state.config.read().await.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(Err(anyhow::anyhow!("runtime: {e}")));
                return;
            }
        };
        let result = rt.block_on(mev_scout_core::jobs::job_doctor(
            &config,
            &mev_scout_core::progress::NoopProgress,
        ));
        let _ = tx.send(result);
    });
    let outcome = rx
        .await
        .map_err(|_| ApiError::internal(anyhow::anyhow!("doctor worker dropped")))?
        .map_err(ApiError::internal)?;
    Ok(Json(outcome))
}

#[derive(Deserialize)]
pub struct ExportQuery {
    pub format: Option<String>,
    pub since: Option<String>,
    pub kinds: Option<String>,
}

async fn export_download(
    State(state): State<SharedState>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let config = state.config.read().await.clone();
    let chain = config.chain;
    let store = mev_scout_core::explorer::store::ExplorerStore::open(
        config.effective_explorer_db_path(&chain),
    )
    .map_err(ApiError::internal)?;
    let format = q.format.as_deref().unwrap_or("json");
    let ops =
        mev_scout_core::jobs::collect_export_ops(&store, q.since.as_deref(), q.kinds.as_deref())
            .map_err(ApiError::internal)?;
    let (body, content_type) =
        mev_scout_core::jobs::format_export_body(&ops, format).map_err(ApiError::internal)?;
    let ext = if format == "csv" { "csv" } else { "json" };
    let headers = [
        (axum::http::header::CONTENT_TYPE, content_type),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"explorer_export.{ext}\""),
        ),
    ];
    Ok((headers, body).into_response())
}

fn map_rejected(r: &rusqlite::Row<'_>) -> rusqlite::Result<RejectedRow> {
    Ok(RejectedRow {
        block_number: r.get::<_, i64>(0)? as u64,
        tx_index: r.get::<_, Option<i64>>(1)?.map(|v| v as u64),
        strategy: r.get(2)?,
        pool_a: r.get(3)?,
        pool_b: r.get(4)?,
        token_in: r.get(5)?,
        token_out: r.get(6)?,
        expected_profit: r.get(7)?,
        gas_cost_wei: r.get(8)?,
        reject_reason: r.get(9)?,
        detail: r.get(10)?,
    })
}

/// Map kind strings used by the frontend filter chips.
#[allow(dead_code)]
pub fn kind_map() -> HashMap<String, MevKind> {
    let mut m = HashMap::new();
    for k in [
        MevKind::ArbAtomic,
        MevKind::Sandwich,
        MevKind::Frontrun,
        MevKind::Backrun,
        MevKind::Liquidation,
        MevKind::Jit,
        MevKind::JitArb,
        MevKind::Unknown,
    ] {
        m.insert(k.as_str().to_string(), k);
    }
    m
}
