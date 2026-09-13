//! ``explorer show`` / ``explorer explain`` - per-transaction detail views.

use super::*;

// ── show / explain ──────────────────────────────────────────────────────

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
        let setup = init_rpc(config, chain, true).await?;
        let h: B256 = tx_hash.parse().context("invalid tx hash")?;
        println!("\nTracing via debug_traceTransaction (prestateTracer diffMode)...");
        match setup.rpc.debug_trace_transaction_prestatediff(h).await {
            Ok(raw) => {
                let note = summarize_prestatediff(&raw);
                println!("  {note}");
                store.mark_trace_verified(tx_hash, None, &note)?;
                println!("  stored trace_verified on the op");
            }
            Err(e) => anyhow::bail!("trace failed (provider may lack debug_*): {e:#}"),
        }
    }
    Ok(())
}

/// Summarize a prestateDiff response: native balance deltas of accounts that
/// appear in the post-state with a balance (exact verification primitive).
fn summarize_prestatediff(raw: &serde_json::Value) -> String {
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

pub async fn cmd_explain(config: &Config, tx_hash: &str) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let ops = store.ops_for_tx(tx_hash)?;
    let Some(op) = ops.first() else {
        anyhow::bail!("no explorer op for tx {tx_hash}");
    };
    let block = op.block_number;
    let rejects =
        store.rejected_in_range(&chain.to_string(), block.saturating_sub(1), block + 1)?;

    println!(
        "Explaining miss for {tx_hash} (block {block}, kind {})",
        op.kind
    );
    println!(
        "  realized: {} USD | route: {}",
        op.profit_usd.unwrap_or(0.0),
        route_text(op)
    );

    // M-taxonomy via the validate engine on a 1-block window.
    let report = validate::compute_validation(&store, chain, block, block, 0, None, false)?;
    let miss = report.miss_distribution();
    println!("  inferred cause (per attribution taxonomy):");
    println!(
        "    M1 pool-gap: {} | M3 threshold: {} | M4 gas: {} | M5 quote: {} | M6 competition: {} | M7 coverage: {}",
        miss.m1_pool_gap,
        miss.m3_pricing_threshold,
        miss.m4_gas_model,
        miss.m5_quote_math,
        miss.m6_competition,
        miss.m7_scanner_coverage
    );

    if !report.missing_pools.is_empty() {
        println!(
            "  pools missing from scanner coverage: {}",
            report.missing_pools.join(", ")
        );
    }

    println!("\n  closest rejected candidates (same/adjacent block):");
    if rejects.is_empty() {
        println!("    (none recorded — run `run`/`live --record-rejections` over this window)");
    }
    for r in rejects.iter().take(10) {
        println!(
            "    blk {} tx {:?} {} reason={} profit={} detail={}",
            r.block_number,
            r.tx_index.unwrap_or(0),
            r.strategy,
            r.reject_reason,
            r.expected_profit.as_deref().unwrap_or("-"),
            r.detail.as_deref().unwrap_or("-"),
        );
    }
    println!("\n  deep verification: mev-scout explorer show {tx_hash} --trace");
    Ok(())
}
