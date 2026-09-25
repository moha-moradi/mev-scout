//! ``explorer show`` - per-transaction detail view.

use super::*;
use mev_scout_core::explorer::store::OpportunityRow;
use mev_scout_core::mev::{mev_verdict, MevVerdict};

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

pub async fn cmd_show(
    config: &Config,
    tx_hash: &str,
    trace: bool,
    tolerance_pct: Option<f64>,
) -> anyhow::Result<()> {
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
        use mev_scout_core::jobs::{job_trace_op, TraceVerdict};

        // `--tolerance-pct` overrides the config on a throwaway clone.
        let cfg = match tolerance_pct {
            Some(p) => {
                let mut c = config.clone();
                c.explorer.trace_tolerance_pct = p;
                c
            }
            None => config.clone(),
        };

        let outcome = job_trace_op(&cfg, tx_hash, &NoopProgress).await?;
        let err = outcome
            .profit_error_pct
            .map(|p| format!("{p:+.1}%"))
            .unwrap_or_else(|| "n/a (expected ≈ 0)".into());
        println!(
            "  trace gate: {} | expected {:?} USD | trace {:?} USD | err {} | tol ±{:.1}%",
            outcome.verdict,
            outcome.expected_profit_usd,
            outcome.trace_profit_usd,
            err,
            outcome.tolerance_pct,
        );
        match &outcome.verdict {
            TraceVerdict::Pass => {}
            TraceVerdict::Unverifiable(reason) => {
                eprintln!("WARN: trace check unverifiable — {reason}");
            }
            TraceVerdict::Fail(reason) => {
                anyhow::bail!("trace gate FAILED: {reason}");
            }
        }
    }

    // MEV verdict (MEV-VERIFICATION §A): detector-expected net profit vs the
    // trace-observed realized native delta for the same tx, joined by tx_hash
    // only. Refetch ops so a just-run `--trace` writes fresh details.
    let ops = store.ops_for_tx(tx_hash)?;
    let opps = store.opportunities_for_tx(tx_hash)?;
    if !opps.is_empty() {
        let opp = &opps[0];
        let verdict = mev_verdict_for(config, tolerance_pct, opp, &ops);
        let expected = expected_net_wei(opp);
        let realized = realized_native_delta_wei(&ops);
        let tol = tolerance_pct.unwrap_or(config.explorer.mev_tolerance_pct);
        println!(
            "  mev gate: {} | expected {expected:?} wei | realized {realized:?} wei | tol ±{tol:.1}%",
            verdict,
        );
        match &verdict {
            MevVerdict::Pass => {}
            MevVerdict::Unverifiable(reason) => {
                eprintln!("WARN: MEV check unverifiable — {reason}");
            }
            MevVerdict::Fail(reason) => {
                anyhow::bail!("MEV gate FAILED: {reason}");
            }
        }
    }
    Ok(())
}

/// Detector-expected net profit in native wei (`expected_profit − gas`).
fn expected_net_wei(opp: &OpportunityRow) -> Option<i128> {
    let profit = opp.expected_profit.as_ref()?.parse::<u128>().ok()?;
    let gas = opp
        .gas_cost_wei
        .as_ref()
        .and_then(|s| s.parse::<u128>().ok())?;
    Some(profit as i128 - gas as i128)
}

/// Trace-observed realized native delta (wei) from the op's details.
fn realized_native_delta_wei(ops: &[MevOpRow]) -> Option<i128> {
    for op in ops {
        let v: serde_json::Value = op
            .details_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        if let Some(s) = v.get("trace_native_delta_wei").and_then(|x| x.as_str()) {
            if let Ok(wei) = s.parse::<i128>() {
                return Some(wei);
            }
        }
    }
    None
}

/// Native USD price implied by the trace job itself
/// (`trace_profit_usd` / |delta wei|), used to turn `mev_error_usd_tol` into a
/// wei band for the degenerate expected ≈ 0 branch. Falls back to $1/native
/// when the ratio isn't recoverable from the op's own details.
fn native_price_usd(ops: &[MevOpRow]) -> Option<f64> {
    for op in ops {
        let v: serde_json::Value = op
            .details_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let usd = v.get("trace_profit_usd").and_then(|x| x.as_f64())?;
        let wei = v
            .get("trace_native_delta_wei")
            .and_then(|x| x.as_str())?
            .parse::<i128>()
            .ok()?;
        if usd.is_finite() && usd >= 0.0 && wei != 0 {
            return Some(usd / wei.abs() as f64);
        }
    }
    None
}

/// Assemble the §A verdict for one detector opportunity vs its realized ops.
fn mev_verdict_for(
    config: &Config,
    tolerance_pct: Option<f64>,
    opp: &OpportunityRow,
    ops: &[MevOpRow],
) -> MevVerdict {
    let tol = tolerance_pct.unwrap_or(config.explorer.mev_tolerance_pct);
    let expected = expected_net_wei(opp);
    let realized = realized_native_delta_wei(ops);
    let abs_wei = match native_price_usd(ops) {
        Some(p) if p > 0.0 => (config.explorer.mev_error_usd_tol / p) as i128,
        _ => (config.explorer.mev_error_usd_tol * 1e18) as i128,
    };
    let err_pct = match (expected, realized) {
        (Some(e), Some(r)) if e != 0 => Some((e - r) as f64 / e.abs() as f64 * 100.0),
        _ => None,
    };
    mev_verdict(expected, realized, err_pct, tol, abs_wei)
}

/// Summarize a prestateDiff response: native balance deltas of accounts that
/// appear in the post-state with a balance (exact verification primitive).
#[allow(dead_code)]
fn summarize_prestatediff(raw: &serde_json::Value) -> String {
    mev_scout_core::jobs::summarize_prestatediff(raw)
}
