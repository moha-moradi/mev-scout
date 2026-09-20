//! ``explorer show`` - per-transaction detail view.

use super::*;

fn route_text(ev: &MevOpRow) -> String {
    let mut hops: Vec<String> = Vec::new();
    if let Some(route) = ev.route_json.as_deref() {
        if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(route)
        {
            for h in arr {
                let pool = h.get("pool").and_then(|v| v.as_str()).unwrap_or("?");
                let tin = h.get("token_in").and_then(|v| v.as_str()).unwrap_or("?");
                let tout = h.get("token_out").and_then(|v| v.as_str()).unwrap_or("?");
                hops.push(format!(
                    "{} ({} -> {})",
                    short_addr(pool),
                    short_addr(tin),
                    short_addr(tout)
                ));
            }
        }
    }
    if hops.is_empty() {
        if let Some(details) = ev.details_json.as_deref() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(details) {
                if let Some(p) = v.get("pool").and_then(|v| v.as_str()) {
                    hops.push(short_addr(p));
                }
            }
        }
    }
    hops.join(" -> ")
}

pub async fn cmd_show(config: &Config, tx_hash: &str, trace: bool) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let ops = store.ops_for_tx(tx_hash)?;
    if ops.is_empty() {
        anyhow::bail!("no explorer ops for tx {tx_hash}");
    }

    for ev in &ops {
        println!("Op #{} — {} (confidence {})", ev.id, ev.kind, ev.confidence);
        println!(
            "  block {} tx {} searcher {}",
            ev.block_number,
            ev.tx_index.unwrap_or(0),
            short_addr(&ev.eoa)
        );
        println!("  route: {}", route_text(ev));
        println!(
            "  profit: {} {} ({:?} USD) | gas {:?} USD | net {:?} USD",
            ev.profit_amount.as_deref().unwrap_or("-"),
            ev.profit_token
                .as_deref()
                .map(short_addr)
                .unwrap_or_else(|| "-".into()),
            ev.profit_usd,
            ev.gas_cost_usd,
            ev.net_profit_usd,
        );
        if let Some(victims) = &ev.victim_hashes {
            println!("  victims: {victims}");
        }
        if let Some(details) = &ev.details_json {
            println!("  details: {details}");
        }
    }

    if trace {
        use crate::job_progress::NoopProgress;
        use mev_scout_core::jobs::job_trace_op;
        let _ = job_trace_op(config, tx_hash, &NoopProgress).await?;
    }
    Ok(())
}

/// Summarize a prestateDiff response: native balance deltas of accounts that
/// appear in the post-state with a balance (exact verification primitive).
#[allow(dead_code)]
fn summarize_prestatediff(raw: &serde_json::Value) -> String {
    mev_scout_core::jobs::summarize_prestatediff(raw)
}
