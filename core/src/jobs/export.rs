//! Bulk export of realized explorer ops (json|csv).

use serde::Serialize;

use crate::config::validation;
use crate::config::Config;
use crate::explorer::store::{ExplorerStore, MevOpRow};
use crate::explorer::MevKind;
use crate::progress::JobProgress;
use crate::utils::epoch_secs;

#[derive(Debug, Clone)]
pub struct ExportOpts {
    pub format: String,
    pub since: Option<String>,
    pub kinds: Option<String>,
    pub arb_shape: Option<String>,
    pub out: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportOutcome {
    pub path: String,
    pub count: usize,
    pub format: String,
}

/// Wrapper that stamps `mode = "realized"` on every exported op so scanner
/// opportunities in the same DB are not conflated (§2.1 / §36).
#[derive(Debug, Clone, Serialize)]
struct ExportOp<'a> {
    mode: &'static str,
    #[serde(flatten)]
    op: &'a MevOpRow,
}

fn since_ts(since: Option<&str>) -> u64 {
    let now = epoch_secs();
    match since {
        Some("1d") => now - 86_400,
        Some("7d") => now - 7 * 86_400,
        Some("30d") => now - 30 * 86_400,
        Some("all") | None => 0,
        Some(other) => {
            tracing::warn!("unknown --since '{other}', using all");
            0
        }
    }
}

fn parse_kinds(s: &str) -> anyhow::Result<Vec<MevKind>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        out.push(MevKind::parse(part).ok_or_else(|| anyhow::anyhow!("unknown kind '{part}'"))?);
    }
    Ok(out)
}

fn matches_arb_shape(op: &MevOpRow, shape: &str) -> bool {
    if op.kind != "arb_atomic" && op.kind != "jit_arb" {
        return false;
    }
    op.details_json
        .as_deref()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .and_then(|v| {
            v.pointer("/arb_meta/arb_shape")
                .and_then(|x| x.as_str())
                .map(|s| s.eq_ignore_ascii_case(shape))
        })
        .unwrap_or(false)
}

fn render_csv(ops: &[MevOpRow]) -> String {
    let mut w = String::from(
        "mode,block_number,tx_index,tx_hash,ts,kind,eoa,confidence,canonical_id,profit_token,profit_amount,profit_usd,gas_cost_usd,net_profit_usd,route_json,victim_hashes,arb_shape\n",
    );
    for o in ops {
        let shape = o
            .details_json
            .as_deref()
            .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
            .and_then(|v| {
                v.pointer("/arb_meta/arb_shape")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        w.push_str(&format!(
            "realized,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            o.block_number,
            o.tx_index.map(|v| v.to_string()).unwrap_or_default(),
            o.tx_hash,
            o.ts,
            o.kind,
            o.eoa,
            o.confidence,
            o.canonical_id.as_deref().unwrap_or(""),
            o.profit_token.as_deref().unwrap_or(""),
            o.profit_amount.as_deref().unwrap_or(""),
            o.profit_usd.map(|v| v.to_string()).unwrap_or_default(),
            o.gas_cost_usd.map(|v| v.to_string()).unwrap_or_default(),
            o.net_profit_usd.map(|v| v.to_string()).unwrap_or_default(),
            o.route_json.as_deref().unwrap_or(""),
            o.victim_hashes.as_deref().unwrap_or(""),
            shape,
        ));
    }
    w
}

/// Collect filtered ops without writing — used by `format_export_body` / CLI export.
pub fn collect_export_ops(
    store: &ExplorerStore,
    since: Option<&str>,
    kinds: Option<&str>,
) -> anyhow::Result<Vec<MevOpRow>> {
    collect_export_ops_filtered(store, since, kinds, None)
}

pub fn collect_export_ops_filtered(
    store: &ExplorerStore,
    since: Option<&str>,
    kinds: Option<&str>,
    arb_shape: Option<&str>,
) -> anyhow::Result<Vec<MevOpRow>> {
    let ts = since_ts(since);
    let kind_filter = kinds.map(parse_kinds).transpose()?;
    let mut ops: Vec<MevOpRow> = match &kind_filter {
        Some(ks) if !ks.is_empty() => store
            .ops_since(ts)?
            .into_iter()
            .filter(|o| ks.iter().any(|k| k.as_str() == o.kind))
            .collect(),
        _ => store.ops_since(ts)?,
    };
    if let Some(shape) = arb_shape {
        ops.retain(|o| matches_arb_shape(o, shape));
    }
    ops.sort_by_key(|o| (o.block_number, o.tx_index.unwrap_or(0)));
    Ok(ops)
}

pub fn format_export_body(ops: &[MevOpRow], format: &str) -> anyhow::Result<(String, String)> {
    if format == "csv" {
        Ok((render_csv(ops), "text/csv".into()))
    } else {
        let wrapped: Vec<ExportOp<'_>> = ops
            .iter()
            .map(|op| ExportOp {
                mode: "realized",
                op,
            })
            .collect();
        Ok((
            serde_json::to_string_pretty(&wrapped)?,
            "application/json".into(),
        ))
    }
}

pub async fn job_export(
    config: &Config,
    opts: &ExportOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<ExportOutcome> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;
    let ops = collect_export_ops_filtered(
        &store,
        opts.since.as_deref(),
        opts.kinds.as_deref(),
        opts.arb_shape.as_deref(),
    )?;

    let format = if opts.format == "csv" { "csv" } else { "json" };
    let out_path = opts
        .out
        .clone()
        .unwrap_or_else(|| format!("results/explorer_export_{}.{}", epoch_secs(), format));
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let (body, _) = format_export_body(&ops, format)?;
    std::fs::write(&out_path, body)?;
    progress.log(&format!("Exported {} ops -> {out_path}", ops.len()));

    Ok(ExportOutcome {
        path: out_path,
        count: ops.len(),
        format: format.to_string(),
    })
}
