//! On-demand debug_traceTransaction verification for an explorer op.
//!
//! Phase 0 (measurement-only): reconcile the classifier's expected USD profit
//! against the trace-observed native balance delta (`summarize_prestatediff` +
//! [`parse_prestatediff_deltas`]). Results land on the op's `details_json`
//! (`trace_verified`, `trace_profit_usd`, `expected_profit_usd`,
//! `profit_error_pct`, `trace_native_delta_wei`). This never feeds
//! `classify_block`.

use alloy::primitives::{Address, B256, I256, U256};
use anyhow::Context;
use serde::Serialize;

use crate::config::validation;
use crate::config::Config;
use crate::explorer::pricing;
use crate::explorer::store::{ExplorerStore, TraceVerification};
use crate::progress::JobProgress;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Serialize)]
pub struct TraceOutcome {
    pub tx_hash: String,
    pub summary: String,
    pub verified: bool,
    /// Classifier-expected USD profit (sum over the tx's ops).
    pub expected_profit_usd: Option<f64>,
    /// Trace-observed native delta converted to USD (signed).
    pub trace_profit_usd: Option<f64>,
    /// `(expected − trace) / expected × 100`.
    pub profit_error_pct: Option<f64>,
}

fn short_addr(s: &str) -> String {
    if s.len() == 42 && s.starts_with("0x") {
        format!("{}..{}", &s[..8], &s[s.len() - 6..])
    } else {
        s.to_string()
    }
}

/// Parse the per-address native balance deltas from a prestate-diff trace.
/// The result is signed (`post − pre`) and uses full-width `I256` (the raw
/// trace carries decimal strings for arbitrary-precision balances).
pub fn parse_prestatediff_deltas(raw: &serde_json::Value) -> Vec<(Address, I256)> {
    let mut out: Vec<(Address, I256)> = Vec::new();
    let pre = raw.get("pre").and_then(|v| v.as_object());
    let post = raw.get("post").and_then(|v| v.as_object());
    let (Some(pre), Some(post)) = (pre, post) else {
        return out;
    };
    for (addr, entry) in post {
        let Ok(address) = addr.parse::<Address>() else {
            continue;
        };
        let post_bal = entry
            .get("balance")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<U256>().ok());
        let pre_bal = pre
            .get(addr)
            .and_then(|e| e.get("balance"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<U256>().ok())
            .unwrap_or(U256::ZERO);
        let Some(pb) = post_bal else { continue };
        out.push((address, I256::from_raw(pb) - I256::from_raw(pre_bal)));
    }
    out
}

/// Signed sum of native deltas over `addrs` (searcher EOA + its contract).
pub fn native_delta_for(raw: &serde_json::Value, addrs: &[Address]) -> I256 {
    let deltas = parse_prestatediff_deltas(raw);
    let mut total = I256::ZERO;
    for (a, d) in &deltas {
        if addrs.contains(a) {
            total += *d;
        }
    }
    total
}

/// Convert a signed wei delta to f64 (precision loss acceptable at report layer).
pub fn i256_to_f64(v: I256) -> f64 {
    let neg = v.is_negative();
    let f = pricing::u256_to_f64(v.wrapping_abs().into_raw());
    if neg {
        -f
    } else {
        f
    }
}

/// Human-readable balance-delta summary (kept for `draw_show`/feedback).
pub fn summarize_prestatediff(raw: &serde_json::Value) -> String {
    let deltas = parse_prestatediff_deltas(raw);
    if deltas.is_empty() {
        return "prestateDiff received (no top-level balance deltas parsed — see raw trace)"
            .to_string();
    }
    let lines: Vec<String> = deltas
        .iter()
        .map(|(addr, d)| {
            format!(
                "  {} native delta {:.6}",
                short_addr(&format!("{addr:#x}")),
                i256_to_f64(*d) / 1e18
            )
        })
        .collect();
    format!("balance deltas:\n{}", lines.join("\n"))
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

    // Expected: sum of the classifier's persisted USD profit for this tx.
    let expected_profit_usd: Option<f64> = {
        let vals: Vec<f64> = ops.iter().filter_map(|o| o.profit_usd).collect();
        if vals.is_empty() {
            None
        } else {
            Some(vals.iter().sum())
        }
    };

    // Trace actual: native balance delta of the searcher EOA + its contract.
    let mut addrs: Vec<Address> = Vec::new();
    for op in &ops {
        if let Ok(a) = op.eoa.parse::<Address>() {
            addrs.push(a);
        }
        if let Some(c) = &op.contract {
            if let Ok(a) = c.parse::<Address>() {
                addrs.push(a);
            }
        }
    }
    addrs.sort();
    addrs.dedup();
    let native_delta = native_delta_for(&raw, &addrs);
    let native_delta_f = i256_to_f64(native_delta);

    // Native USD at the block's hour (cached price, else wrapped-native, else Llama).
    let ts = ops[0].ts;
    let hour = pricing::hour_bucket(ts);
    let (_, chain_cfg) = validation::resolve_chain(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let wrapped_native = chain_cfg.wrapped_native_token.unwrap_or(Address::ZERO);
    let native_usd = match store.price_at(Address::ZERO, hour)? {
        Some((p, _)) => Some(p),
        None => {
            let mut p = store.price_at(wrapped_native, hour)?.map(|(p, _)| p);
            if p.is_none() && !wrapped_native.is_zero() {
                p = pricing::fetch_native_price_llama(chain, wrapped_native)
                    .await
                    .ok();
            }
            p
        }
    };
    let trace_profit_usd = native_usd.map(|p| native_delta_f / 1e18 * p);

    let profit_error_pct = match (expected_profit_usd, trace_profit_usd) {
        (Some(e), Some(t)) if e.abs() > f64::EPSILON => Some((e - t) / e * 100.0),
        _ => None,
    };

    let verification = TraceVerification {
        expected_profit_usd,
        trace_profit_usd,
        profit_error_pct,
        native_delta_wei: native_delta.to_string(),
        note: summary.clone(),
    };
    store.mark_trace_verified(tx_hash, &verification)?;
    progress.log("stored trace_verified + profit_error_pct on the op");

    Ok(TraceOutcome {
        tx_hash: tx_hash.to_string(),
        summary,
        verified: true,
        expected_profit_usd,
        trace_profit_usd,
        profit_error_pct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use serde_json::json;

    fn raw_with(pre_bal: &str, post_bal: &str) -> serde_json::Value {
        json!({
            "pre": { "0x1111111111111111111111111111111111111111": { "balance": pre_bal } },
            "post": { "0x1111111111111111111111111111111111111111": { "balance": post_bal } }
        })
    }

    #[test]
    fn parses_signed_native_delta() {
        let a = address!("1111111111111111111111111111111111111111");
        let raw = raw_with("1000000000000000000", "3000000000000000000");
        let deltas = parse_prestatediff_deltas(&raw);
        assert_eq!(
            deltas,
            vec![(a, I256::try_from(2_000_000_000_000_000_000i128).unwrap())]
        );
        assert_eq!(native_delta_for(&raw, &[a]), deltas[0].1);
        assert!(native_delta_for(&raw, &[Address::ZERO]).is_zero());
    }

    #[test]
    fn parses_negative_delta_and_new_account() {
        let raw = raw_with("5000000000000000000", "4500000000000000000");
        assert_eq!(
            parse_prestatediff_deltas(&raw)[0].1,
            I256::try_from(-500_000_000_000_000_000i128).unwrap()
        );
        // post-only (fresh account) => delta = post balance
        let fresh = json!({
            "pre": {},
            "post": { "0x2222222222222222222222222222222222222222": { "balance": "7" } }
        });
        let d = parse_prestatediff_deltas(&fresh);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].1, I256::try_from(7i128).unwrap());
    }

    #[test]
    fn summarize_includes_delta() {
        let raw = raw_with("0", "1000000000000000000");
        let s = summarize_prestatediff(&raw);
        assert!(s.contains("native delta 1.000000"), "{s}");
    }
}
