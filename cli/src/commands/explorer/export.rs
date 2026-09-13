//! ``explorer export`` - dump indexed opportunities to CSV/JSON.

use super::*;

// ── export ──────────────────────────────────────────────────────────────

pub async fn cmd_export(
    config: &Config,
    since: Option<&str>,
    kinds: Option<&str>,
    format: &str,
    out: Option<&str>,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
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

    let out_path = out.map(str::to_string).unwrap_or_else(|| {
        format!(
            "results/explorer_export_{}.{}",
            epoch_secs(),
            if format == "csv" { "csv" } else { "json" }
        )
    });
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }

    if format == "csv" {
        let mut w = String::from(
            "block_number,tx_index,tx_hash,ts,kind,eoa,confidence,canonical_id,profit_token,profit_amount,profit_usd,gas_cost_usd,net_profit_usd,route_json,victim_hashes\n",
        );
        for o in &ops {
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
        std::fs::write(&out_path, w)?;
    } else {
        let json = serde_json::to_string_pretty(&ops)?;
        std::fs::write(&out_path, json)?;
    }
    println!("Exported {} ops -> {}", ops.len(), out_path);
    Ok(())
}
