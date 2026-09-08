//! `mev-scout explorer` subcommands (plan §10):
//! doctor, index, live, stats, top, show, explain, validate, export.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alloy::primitives::{B256, U256};
use anyhow::Context;
use comfy_table::Table;

use crate::rpc_setup::init_rpc;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::explorer::ingest::{backfill_range, run_live, safe_head, IngestConfig};
use mev_scout_core::explorer::store::{ExplorerStore, MevOpRow};
use mev_scout_core::explorer::validate;
use mev_scout_core::explorer::MevKind;
use mev_scout_core::types::ChainName;
use mev_scout_core::utils::epoch_secs;

// ── shared setup ────────────────────────────────────────────────────────

fn explorer_store(config: &Config, chain: ChainName) -> anyhow::Result<ExplorerStore> {
    ExplorerStore::open(config.effective_explorer_db_path(&chain))
}

fn ingest_config(config: &Config, chain: ChainName) -> anyhow::Result<IngestConfig> {
    let (_, chain_cfg) =
        validation::resolve_chain(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;
    Ok(cfg)
}

fn parse_kinds(s: &str) -> anyhow::Result<Vec<MevKind>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        out.push(
            MevKind::parse(part)
                .ok_or_else(|| anyhow::anyhow!("unknown kind '{part}'"))?,
        );
    }
    Ok(out)
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

fn short_addr(s: &str) -> String {
    if s.len() == 42 && s.starts_with("0x") {
        format!("{}..{}", &s[..8], &s[s.len() - 6..])
    } else {
        s.to_string()
    }
}

fn short_err(e: &anyhow::Error) -> String {
    format!("{e:#}").chars().take(40).collect()
}

fn parse_duration(s: &str) -> anyhow::Result<std::time::Duration> {
    humantime::parse_duration(s).with_context(|| format!("invalid duration '{s}'"))
}

fn stop_flag_with_deadline(deadline: Option<std::time::Duration>) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_ctrl = stop.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        stop_ctrl.store(true, Ordering::Relaxed);
    });
    if let Some(d) = deadline {
        let stop_d = stop.clone();
        tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if t0.elapsed() >= d {
                    stop_d.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });
    }
    stop
}

// ── doctor (Phase 0 capability probe) ───────────────────────────────────

pub async fn cmd_doctor(config: &Config) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    println!("Explorer doctor — {chain} (chain id {})", chain.chain_id());

    let setup = init_rpc(config, chain, false).await?;
    let providers = &setup.provider_configs;

    let mut table = Table::new();
    table.set_header(vec!["provider", "latest", "archive", "bulk-receipts", "traces", "rps"]);
    for (url, rps, archive_flag) in providers.iter() {
        let shown = if url.len() > 40 {
            format!("{}..", &url[..38])
        } else {
            url.clone()
        };
        let urls = [url.as_str()];

        let latest = match mev_scout_core::rpc::RpcClient::from_urls(&urls, chain.chain_id()) {
            Ok(c) => c
                .get_block_number()
                .await
                .map(|n| n.to_string())
                .unwrap_or_else(|_| "x".into()),
            Err(_) => "x".into(),
        };
        let archive = if *archive_flag { "config" } else { "n/a" };
        let bulk = match mev_scout_core::rpc::RpcClient::from_urls(&urls, chain.chain_id()) {
            Ok(c) => {
                let tip = c.get_block_number().await.unwrap_or(0);
                if tip > 1 {
                    match c.get_receipts(tip - 1).await {
                        Ok(_) => "ok".into(),
                        Err(e) => format!("x {}", short_err(&e)),
                    }
                } else {
                    "x".into()
                }
            }
            Err(_) => "x".into(),
        };
        let traces = match mev_scout_core::rpc::RpcClient::from_urls(&urls, chain.chain_id()) {
            Ok(c) => {
                let tip = c.get_block_number().await.unwrap_or(0);
                let tx_hash = if tip > 2 {
                    c.get_block(tip - 2)
                        .await
                        .ok()
                        .and_then(|(_, txs)| txs.first().map(|t| t.hash))
                } else {
                    None
                };
                match tx_hash {
                    Some(h) => match c.debug_trace_transaction_prestatediff(h).await {
                        Ok(_) => "ok".into(),
                        Err(e) => format!("x {}", short_err(&e)),
                    },
                    None => "n/a".into(),
                }
            }
            Err(_) => "x".into(),
        };
        table.add_row(vec![
            shown,
            latest,
            archive.to_string(),
            bulk,
            traces,
            rps.map(|r| format!("{r}")).unwrap_or_else(|| "-".into()),
        ]);
    }
    println!("{table}");
    println!("\nGate: Phase 0 requires at least one provider with latest + bulk-receipts support.");
    Ok(())
}

// ── index (backfill + live indexing) ────────────────────────────────────

pub async fn cmd_index(
    config: &Config,
    from: Option<u64>,
    to: Option<u64>,
    days: Option<u64>,
    live: bool,
    duration: Option<&str>,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let setup = init_rpc(config, chain, true).await?;
    let store = explorer_store(config, chain)?;
    let cfg = ingest_config(config, chain)?;

    if live {
        let deadline = duration.map(parse_duration).transpose()?;
        let stop = stop_flag_with_deadline(deadline);
        let t0 = std::time::Instant::now();
        let indexed = run_live(
            &setup.rpc,
            &store,
            &cfg,
            config.explorer.poll_interval_ms,
            stop,
        )
        .await?;
        println!(
            "Live indexing done — {} blocks indexed, {} ops total in {:.1}s (db: {})",
            indexed,
            store.op_count_since(0)?,
            t0.elapsed().as_secs_f64(),
            config.effective_explorer_db_path(&chain),
        );
        return Ok(());
    }

    // Historical backfill: resolve range.
    let head = safe_head(&setup.rpc, &cfg).await?;
    let (from, to) = match (from, to) {
        (Some(f), Some(t)) => (f, t),
        (None, None) => {
            let d = days.unwrap_or(30);
            let blocks_per_day = 86_400 / block_time_secs(chain.chain_id()).max(1);
            let n = d.saturating_mul(blocks_per_day).max(1);
            (head.saturating_sub(n), head)
        }
        _ => anyhow::bail!("--from and --to must be used together (or use --days)"),
    };
    if to > head {
        anyhow::bail!(
            "--to {to} is beyond safe head {head} (head minus {} confirmations)",
            cfg.confirmations
        );
    }

    println!("Indexing blocks {from}-{to} ({} blocks) — {chain}", to - from + 1);
    let t0 = std::time::Instant::now();
    let (blocks_done, ops) = backfill_range(
        &setup.rpc,
        &store,
        &cfg,
        from,
        to,
        config.explorer.checkpoint_every,
    )
    .await?;
    println!(
        "Indexed {blocks_done} blocks, {ops} ops in {:.1}s -> {}",
        t0.elapsed().as_secs_f64(),
        config.effective_explorer_db_path(&chain),
    );
    Ok(())
}

fn block_time_secs(chain_id: u64) -> u64 {
    match chain_id {
        1 => 12,
        137 | 43114 => 2,
        56 | 42161 | 8453 | 10 => 1,
        _ => 2,
    }
}

// ── live feed (mev.zone-style, store tail) ──────────────────────────────

pub async fn cmd_live_feed(
    config: &Config,
    kinds: Option<&str>,
    min_profit_usd: f64,
    poll_interval_ms: u64,
    duration: Option<&str>,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let kinds = match kinds {
        Some(s) => parse_kinds(s)?,
        None => vec![],
    };
    let deadline = duration.map(parse_duration).transpose()?;

    let _ = stop_flag_with_deadline(deadline); // Ctrl+C handling
    let t0 = std::time::Instant::now();

    println!(
        "Explorer live feed — {chain} (tail of the indexed store; run `explorer index --live` alongside)"
    );

    let mut cursor = store.op_count_since(0)?;
    loop {
        if let Some(dl) = deadline {
            if t0.elapsed() >= dl {
                break;
            }
        }
        // Drain new ops since the last poll (simple id-based tail).
        let total = store.op_count_since(0)?;
        if total > cursor {
            let take = (total - cursor).min(20) as usize;
            let feed = store.feed_tail(take, &kinds)?;
            for row in feed.iter().rev() {
                if row.profit_usd.unwrap_or(0.0) < min_profit_usd {
                    continue;
                }
                println!(
                    "{}  blk {:>9}  {:<11}  {:<12}  ${:>10.2}  {}",
                    time_hhmmss(row.ts),
                    row.block_number,
                    row.kind,
                    row.profit_token.as_deref().map(short_addr).unwrap_or_else(|| "-".into()),
                    row.net_profit_usd.or(row.profit_usd).unwrap_or(0.0),
                    short_addr(&row.eoa),
                );
            }
            cursor = total;
        }
        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms.max(250))).await;
    }
    Ok(())
}

fn time_hhmmss(ts: u64) -> String {
    let secs_of_day = ts % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

// ── stats / top ─────────────────────────────────────────────────────────

pub async fn cmd_stats(
    config: &Config,
    since: Option<&str>,
    window: Option<&str>,
    kind: Option<&str>,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let ts = since_ts(since);

    let overview = store.stats_overview(ts)?;
    println!("Explorer stats — {chain} since={}", since.unwrap_or("all"));
    println!(
        "  ops: {} | searchers: {} | gross: ${:.2} | gas: ${:.2} | net: ${:.2} | highest single: ${:.2}",
        overview.ops,
        overview.searchers,
        overview.gross_usd,
        overview.gas_usd,
        overview.net_usd,
        overview.highest_single_usd,
    );

    let _kind_filter = kind.and_then(MevKind::parse);

    println!("\nPer-kind:");
    let rows = store.stats_by_kind(ts)?;
    print_stats_table(&rows);

    println!("\nDaily breakdown:");
    let rows = store.stats_daily(ts)?;
    print_stats_table(&rows);

    if matches!(window, Some("week") | Some("month") | Some("year")) {
        println!(
            "\n(window {:?} — daily breakdown shown; period rollups use the same store views)",
            window
        );
    }

    println!("\nTop searchers (7d):");
    let rows = store.top_senders(since_ts(Some("7d")), 10)?;
    print_stats_table(&rows);

    println!("\nTop pools:");
    let rows = store.top_pools(ts, 10)?;
    print_stats_table(&rows);
    Ok(())
}

fn print_stats_table(rows: &[mev_scout_core::explorer::store::StatsRow]) {
    let mut table = Table::new();
    table.set_header(vec!["label", "ops", "gross USD", "net USD"]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(vec![
            short_addr(&r.label),
            r.ops.to_string(),
            format!("{:.2}", r.gross_usd),
            format!("{:.2}", r.net_usd),
        ]);
    }
    println!("{table}");
}

pub async fn cmd_top(
    config: &Config,
    by: &str,
    metric: &str,
    since: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let ts = since_ts(since);
    let rows = match by {
        "sender" => store.top_senders(ts, limit)?,
        "token" => store.top_tokens(ts, limit)?,
        "pool" => store.top_pools(ts, limit)?,
        other => anyhow::bail!("unknown --by '{other}' (sender|token|pool)"),
    };
    let metric_col = if metric == "ops" { "ops" } else { "gross USD" };
    println!(
        "Explorer top — {chain} by {by} ({metric_col}) since={}",
        since.unwrap_or("all")
    );
    print_stats_table(&rows);
    Ok(())
}

// ── show / explain ──────────────────────────────────────────────────────

fn route_text(ev: &MevOpRow) -> String {
    let mut hops: Vec<String> = Vec::new();
    if let Some(route) = ev.route_json.as_deref() {
        if let Ok(serde_json::Value::Array(arr)) =
            serde_json::from_str::<serde_json::Value>(route)
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
                let signed = pb.as_limbs()[0] as i128
                    + ((pb.as_limbs()[1] as i128) << 64)
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
    let report =
        validate::compute_validation(&store, chain, block, block, 0, None, false)?;
    let miss = report.miss_distribution();
    println!("  inferred cause (per §11.1.2):");
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

// ── validate ────────────────────────────────────────────────────────────

pub async fn cmd_validate(
    config: &Config,
    since: Option<&str>,
    match_window: u64,
    run_ids: Option<Vec<String>>,
    threshold_sweep: bool,
    emit_missing_pools: bool,
    json: bool,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;

    let ts = since_ts(since);
    let (from_block, to_block) = block_window(&store, ts)?;

    let report = validate::compute_validation(
        &store,
        chain,
        from_block,
        to_block,
        match_window,
        run_ids.as_deref(),
        threshold_sweep,
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", validate::render_validation_report(&report));
    }

    if emit_missing_pools && !report.missing_pools.is_empty() {
        let path = "results/missing_pools.txt";
        std::fs::create_dir_all("results").ok();
        std::fs::write(path, report.missing_pools.join("\n"))?;
        println!("\nMissing pools written to {path}");
    }
    Ok(())
}

/// Block window covering `since_ts`: [min, max] block_number in mev_ops.
fn block_window(store: &ExplorerStore, since_ts: u64) -> anyhow::Result<(u64, u64)> {
    let ops = store.ops_since(since_ts)?;
    let mut blocks: Vec<u64> = ops.iter().map(|o| o.block_number).collect();
    if blocks.is_empty() {
        anyhow::bail!("no indexed ops in the requested window — run `explorer index` first");
    }
    blocks.sort_unstable();
    Ok((blocks[0], blocks[blocks.len() - 1]))
}

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
