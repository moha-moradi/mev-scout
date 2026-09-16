//! ``explorer index`` - backfill + live indexing loop, and the live feed renderer.

use super::*;
use crate::job_progress::JobProgress;

// ── index (backfill + live indexing) ────────────────────────────────────

pub async fn cmd_index(
    config: &Config,
    from: Option<u64>,
    to: Option<u64>,
    days: Option<u64>,
    live: bool,
    duration: Option<&str>,
    progress: &dyn JobProgress,
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
        progress.log(&format!(
            "Live indexing done — {} blocks indexed, {} ops total in {:.1}s (db: {})",
            indexed,
            store.op_count_since(0)?,
            t0.elapsed().as_secs_f64(),
            config.effective_explorer_db_path(&chain),
        ));
        return Ok(());
    }

    // Historical backfill: resolve range.
    let head = safe_head(&setup.rpc, &cfg).await?;
    let (from, to) = match (from, to) {
        (Some(f), Some(t)) => (f, t),
        (None, None) => {
            let d = days.unwrap_or(30);
            let blocks_per_day = mev_scout_core::chain::timing::blocks_per_day(chain);
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

    progress.log(&format!(
        "Indexing blocks {from}-{to} ({} blocks) — {chain}",
        to - from + 1
    ));
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
    progress.log(&format!(
        "Indexed {blocks_done} blocks, {ops} ops in {:.1}s -> {}",
        t0.elapsed().as_secs_f64(),
        config.effective_explorer_db_path(&chain),
    ));
    Ok(())
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

    let stop = stop_flag_with_deadline(deadline); // Ctrl+C handling
    let t0 = std::time::Instant::now();

    println!(
        "Explorer live feed — {chain} (tail of the indexed store; run `explorer index --live` alongside)"
    );

    let mut cursor = store.op_count_since(0)?;
    loop {
        if stop.load(Ordering::Relaxed) {
            println!("\n(stop requested — closing feed)");
            break;
        }
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
                    row.profit_token
                        .as_deref()
                        .map(short_addr)
                        .unwrap_or_else(|| "-".into()),
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
