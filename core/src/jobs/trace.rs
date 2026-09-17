//! On-demand debug_traceTransaction verification for an explorer op.

use alloy::primitives::{B256, U256};
use anyhow::Context;
use serde::Serialize;

use crate::config::validation;
use crate::config::Config;
use crate::explorer::store::ExplorerStore;
use crate::progress::JobProgress;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Serialize)]
pub struct TraceOutcome {
    pub tx_hash: String,
    pub summary: String,
    pub verified: bool,
}

fn short_addr(s: &str) -> String {
    if s.len() == 42 && s.starts_with("0x") {
        format!("{}..{}", &s[..8], &s[s.len() - 6..])
    } else {
        s.to_string()
    }
}

pub fn summarize_prestatediff(raw: &serde_json::Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    let pre = raw.get("pre").and_then(|v| v.as_object());
    let post = raw.get("post").and_then(|v| v.as_object());
    if let (Some(pre), Some(post)) = (pre, post) {
        for (addr, entry) in post {
            let post_bal = entry
                .get("balance")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<U256>().ok());
            let pre_bal = pre
                .get(addr)
                .and_then(|e| e.get("balance"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<U256>().ok());
            if let (Some(pb), Some(prb)) = (post_bal, pre_bal) {
                let signed = pb.as_limbs()[0] as i128 + ((pb.as_limbs()[1] as i128) << 64)
                    - prb.as_limbs()[0] as i128
                    - ((prb.as_limbs()[1] as i128) << 64);
                lines.push(format!(
                    "  {} native delta {:.6}",
                    short_addr(addr),
                    signed as f64 / 1e18
                ));
            }
        }
    }
    if lines.is_empty() {
        "prestateDiff received (no top-level balance deltas parsed — see raw trace)".to_string()
    } else {
        format!("balance deltas:\n{}", lines.join("\n"))
    }
}

pub async fn job_trace_op(
    config: &Config,
    tx_hash: &str,
    progress: &dyn JobProgress,
) -> anyhow::Result<TraceOutcome> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;
    let ops = store.ops_for_tx(tx_hash)?;
    if ops.is_empty() {
        anyhow::bail!("no explorer ops for tx {tx_hash}");
    }

    let setup = init_rpc(config, chain, true).await?;
    let h: B256 = tx_hash.parse().context("invalid tx hash")?;
    progress.log("Tracing via debug_traceTransaction (prestateTracer diffMode)...");
    let raw = setup
        .rpc
        .debug_trace_transaction_prestatediff(h)
        .await
        .with_context(|| format!("trace failed (provider may lack debug_*): {tx_hash}"))?;
    let summary = summarize_prestatediff(&raw);
    progress.log(&summary);
    store.mark_trace_verified(tx_hash, None, &summary)?;
    progress.log("stored trace_verified on the op");

    Ok(TraceOutcome {
        tx_hash: tx_hash.to_string(),
        summary,
        verified: true,
    })
}
