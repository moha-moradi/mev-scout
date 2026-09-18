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
    pub out: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportOutcome {
    pub path: String,
    pub count: usize,
    pub format: String,
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

fn render_csv(ops: &[MevOpRow]) -> String {
    let mut w = String::from(
        "block_number,tx_index,tx_hash,ts,kind,eoa,confidence,canonical_id,profit_token,profit_amount,profit_usd,gas_cost_usd,net_profit_usd,route_json,victim_hashes\n",
    );
    for o in ops {
        w.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
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
        ));
    }
    w
}

/// Collect filtered ops without writing — used by API download endpoint.
pub fn collect_export_ops(
    store: &ExplorerStore,
    since: Option<&str>,
    kinds: Option<&str>,
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
    ops.sort_by_key(|o| (o.block_number, o.tx_index.unwrap_or(0)));
    Ok(ops)
}

pub fn format_export_body(ops: &[MevOpRow], format: &str) -> anyhow::Result<(String, String)> {
    if format == "csv" {
        Ok((render_csv(ops), "text/csv".into()))
    } else {
        Ok((
            serde_json::to_string_pretty(ops)?,
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
    let ops = collect_export_ops(&store, opts.since.as_deref(), opts.kinds.as_deref())?;

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
