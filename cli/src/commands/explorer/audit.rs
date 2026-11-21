//! `explorer audit` — opt-in transaction audit with a trace-capable RPC.
//!
//! Never called from ingest: public endpoints usually reject
//! `debug_traceTransaction`/`trace_transaction`, so the command records every
//! attempt and writes whatever the explorer store already knows even when no
//! provider serves a trace.
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use alloy::primitives::B256;
use mev_scout_core::config::Config;
use mev_scout_core::explorer::store::ExplorerStore;
use serde_json::{json, Value};

use crate::cli::AuditArgs;

pub async fn cmd_audit(
    config: &Config,
    args: &AuditArgs,
    out: Option<PathBuf>,
) -> anyhow::Result<()> {
    let tx = B256::from_str(&args.tx)
        .map_err(|_| anyhow::anyhow!("invalid --tx hash '{}' (expected 0x + 64 hex)", args.tx))?;
    let tx_hash = format!("{tx:#x}");
    let db = super::resolved_db_path(config);
    let store = ExplorerStore::open_read_only(&db)?;
    let conn = store.connection();

    let mut stmt = conn.prepare(
        "SELECT id, block_number, tx_index, kind, COALESCE(subkind,''), confidence,
                net_profit_usd, COALESCE(details_json,'{}')
         FROM mev_ops WHERE lower(tx_hash) = ?1 ORDER BY id",
    )?;
    let ops: Vec<Value> = stmt
        .query_map([tx_hash.as_str()], |r| {
            let details: String = r.get(7)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "block": r.get::<_, i64>(1)?,
                "tx_index": r.get::<_, Option<i64>>(2)?,
                "kind": r.get::<_, String>(3)?,
                "subkind": r.get::<_, String>(4)?,
                "confidence": r.get::<_, String>(5)?,
                "net_usd": r.get::<_, Option<f64>>(6)?,
                "details": serde_json::from_str::<Value>(&details).unwrap_or(Value::Null),
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let evidence: Option<Value> = conn
        .query_row(
            "SELECT payload_json FROM tx_evidence WHERE lower(tx_hash) = ?1",
            [tx_hash.as_str()],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());

    let (trace, attempts) = fetch_trace(config, &tx_hash).await;
    let summary = trace.as_ref().map(summarize_trace);
    let hint = trace.is_none().then_some(
        "no provider served a trace; configure a trace-capable RPC (archive) or retry later",
    );
    let report = json!({
        "tx": tx_hash,
        "ops": ops,
        "has_evidence": evidence.is_some(),
        "route": evidence.as_ref()
            .and_then(|e| e.get("swap_events").cloned())
            .unwrap_or(Value::Null),
        "trace": trace,
        "attempts": attempts,
        "summary": summary,
        "hint": hint,
        "evidence": evidence,
    });

    let out = out
        .unwrap_or_else(|| PathBuf::from("results").join(format!("trace-{}.json", &tx_hash[2..])));
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
    println!("audit written: {}", out.display());
    if hint.is_some() {
        tracing::warn!(
            tx = %tx_hash,
            attempts = report["attempts"].as_array().map_or(0, |a| a.len()),
            "trace unavailable on every configured provider"
        );
    }
    Ok(())
}

async fn fetch_trace(config: &Config, tx_hash: &str) -> (Option<Value>, Vec<Value>) {
    let mut attempts = Vec::new();
    let Ok(urls) = config.effective_rpc_urls(config.chain) else {
        return (None, attempts);
    };
    let client = mev_scout_core::utils::http_client(Duration::from_secs(30));
    for url in urls {
        for (method, params) in [
            (
                "debug_traceTransaction",
                json!([tx_hash, {"tracer": "callTracer", "tracerConfig": {"withLog": true}}]),
            ),
            ("trace_transaction", json!([tx_hash])),
        ] {
            match rpc_call(&client, &url, method, params).await {
                Ok(result) if !result.is_null() => {
                    attempts.push(json!({
                        "provider": redact_url(&url),
                        "method": method,
                        "ok": true,
                    }));
                    return (
                        Some(json!({
                            "provider": redact_url(&url),
                            "method": method,
                            "result": result,
                        })),
                        attempts,
                    );
                }
                Ok(_) => attempts.push(json!({
                    "provider": redact_url(&url),
                    "method": method,
                    "ok": false,
                    "error": "null result",
                })),
                Err(e) => attempts.push(json!({
                    "provider": redact_url(&url),
                    "method": method,
                    "ok": false,
                    "error": redact_text(&e, &url),
                })),
            }
        }
    }
    (None, attempts)
}

async fn rpc_call(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "non-JSON response: {}",
            text.chars().take(160).collect::<String>()
        )
    })?;
    if let Some(err) = value.get("error") {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown RPC error");
        return Err(message.to_string());
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

/// Keep `scheme://host` only: provider paths routinely embed API keys.
fn redact_url(url: &str) -> String {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        ("https://", r)
    } else if let Some(r) = url.strip_prefix("http://") {
        ("http://", r)
    } else {
        ("", url)
    };
    let host = rest.split('/').next().unwrap_or("");
    format!("{scheme}{host}/…")
}

fn redact_text(text: &str, url: &str) -> String {
    text.replace(url, &redact_url(url))
        .chars()
        .take(300)
        .collect()
}

fn summarize_trace(trace: &Value) -> Value {
    let mut calls = 0usize;
    let mut targets = HashSet::new();
    let mut selectors: BTreeMap<String, usize> = BTreeMap::new();
    let mut v3_callback = false;
    if let Some(result) = trace.get("result") {
        if result.is_array() {
            // Parity/OpenEthereum trace_transaction.
            for t in result.as_array().into_iter().flatten() {
                let action = t.get("action").unwrap_or(t);
                if let Some(to) = action.get("to").and_then(|v| v.as_str()) {
                    targets.insert(to.to_ascii_lowercase());
                }
                if let Some(sel) = selector(action.get("input")) {
                    *selectors.entry(sel.clone()).or_default() += 1;
                    v3_callback |= is_v3_callback(&sel);
                }
                calls += 1;
            }
        } else {
            walk_call(
                result,
                &mut calls,
                &mut targets,
                &mut selectors,
                &mut v3_callback,
            );
        }
    }
    json!({
        "call_count": calls,
        "unique_targets": targets.len(),
        "selectors": selectors,
        "v3_swap_callback": v3_callback,
    })
}

fn walk_call(
    node: &Value,
    calls: &mut usize,
    targets: &mut HashSet<String>,
    selectors: &mut BTreeMap<String, usize>,
    v3_callback: &mut bool,
) {
    *calls += 1;
    if let Some(to) = node.get("to").and_then(|v| v.as_str()) {
        targets.insert(to.to_ascii_lowercase());
    }
    if let Some(sel) = selector(node.get("input")) {
        *selectors.entry(sel.clone()).or_default() += 1;
        *v3_callback |= is_v3_callback(&sel);
    }
    if let Some(children) = node.get("calls").and_then(|v| v.as_array()) {
        for child in children {
            walk_call(child, calls, targets, selectors, v3_callback);
        }
    }
}

fn selector(input: Option<&Value>) -> Option<String> {
    let s = input?.as_str()?;
    (s.len() >= 10).then(|| s[..10].to_ascii_lowercase())
}

/// Uniswap V3 `uniswapV3SwapCallback(int256,int256,bytes)`.
fn is_v3_callback(selector: &str) -> bool {
    selector == "0xfa461e33"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_provider_paths() {
        assert_eq!(
            redact_url("https://api.example.com/secret-key/ext/bc/C/rpc"),
            "https://api.example.com/…"
        );
        assert!(redact_text("failed at https://a/b?k=1", "https://a/b?k=1").contains("https://a/…"));
    }

    #[test]
    fn summarizes_a_call_tree() {
        let trace = json!({
            "result": {
                "to": "0xpool",
                "input": "0xabcdef1234",
                "calls": [
                    {"to": "0xexec", "input": "0xfa461e330000"},
                    {"to": "0xpool2", "input": "0x12345678"}
                ]
            }
        });
        let s = summarize_trace(&trace);
        assert_eq!(s["call_count"], 3);
        assert_eq!(s["unique_targets"], 3);
        assert_eq!(s["v3_swap_callback"], true);
    }
}
