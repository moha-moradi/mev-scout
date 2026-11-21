//! Scratch read-only audit helper: dump explorer ops with their tx evidence.
//!
//! Usage: cargo run -p mev-scout-cli --example opaudit -- <db> <from> <to> [--full]
//!
//! One JSON line per op. Without `--full` only a compact evidence summary is
//! printed; with `--full` the stored payload is included verbatim.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use mev_scout_core::explorer::store::ExplorerStore;
use serde_json::{json, Value};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let db = args
        .next()
        .expect("usage: opaudit <db> <from> <to> [--full]");
    let from: i64 = args.next().expect("from").parse()?;
    let to: i64 = args.next().expect("to").parse()?;
    let rest: Vec<String> = args.collect();
    let full = rest.iter().any(|a| a == "--full");
    let only_tx: Option<String> = rest
        .iter()
        .find(|a| a.starts_with("0x") && a.len() == 66)
        .map(|a| a.to_ascii_lowercase());

    let store = ExplorerStore::open_read_only(&db)?;
    let conn = store.connection();
    let mut stmt = conn.prepare(
        "SELECT id, block_number, tx_index, tx_hash, kind, COALESCE(subkind,''), confidence,
                COALESCE(net_profit_usd,0.0), COALESCE(profit_usd,0.0),
                COALESCE(details_json,'{}')
         FROM mev_ops WHERE block_number BETWEEN ?1 AND ?2
         ORDER BY block_number, tx_index, id",
    )?;
    let rows = stmt
        .query_map([from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, f64>(8)?,
                r.get::<_, String>(9)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (id, block, tx_index, tx_hash, kind, sub, conf, net, gross, details) in rows {
        if let Some(want) = &only_tx {
            if tx_hash.to_ascii_lowercase() != *want {
                continue;
            }
        }
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM tx_evidence WHERE tx_hash = ?1",
                [&tx_hash],
                |r| r.get(0),
            )
            .ok();
        let mut ev = json!(null);
        if let Some(raw) = &payload {
            let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
            let t = &v["transaction"];
            let input = t["input"].as_str().unwrap_or("");
            ev = json!({
                "from": t["from"],
                "to": t["to"],
                "value": t["value"],
                "selector": input.chars().take(10).collect::<String>(),
                "status": v["receipt"]["status"],
                "logs": v["event_logs"].as_array().map(|a| a.len()),
                "transfers": v["token_transfers"].as_array().map(|a| a.len()),
                "swaps": v["swap_events"].as_array().map(|a| a.len()),
                "unmapped": v["unmapped_logs"].as_array().map(|a| a.len()),
                "first_transfers": v["token_transfers"].as_array().map(|a| a.iter().take(4).cloned().collect::<Vec<_>>()),
                "payload": full.then(|| v.clone()),
            });
        }
        let details: Value = serde_json::from_str(&details).unwrap_or(Value::Null);
        println!(
            "{}",
            json!({
                "id": id,
                "block": block,
                "tx_index": tx_index,
                "tx": tx_hash,
                "kind": kind,
                "subkind": sub,
                "confidence": conf,
                "net": net,
                "gross": gross,
                "details": details,
                "evidence": ev,
            })
        );
    }
    Ok(())
}
