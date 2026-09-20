//! ``explorer index`` - live indexing loop, and the live feed renderer.

use super::*;
use crate::job_progress::JobProgress;
use mev_scout_core::jobs::{job_index, IndexOpts};

pub async fn cmd_index(
    config: &Config,
    duration: Option<&str>,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let opts = IndexOpts {
        duration: duration.map(String::from),
    };
    job_index(config, &opts, progress).await?;
    Ok(())
}

// ── live feed (mev.zone-style, store tail) ──────────────────────────────

pub async fn cmd_live_feed(
    config: &Config,
    kinds: Option<&str>,
    min_profit_usd: f64,
    poll_interval_ms: u64,
    duration: Option<&str>,
    arb_shape: Option<&str>,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let kinds = match kinds {
        Some(s) => parse_kinds(s)?,
        None if !config.explorer.live_feed_kinds.is_empty() => {
            parse_kinds(&config.explorer.live_feed_kinds)?
        }
        // Phase 3 ship gate: default live feed excludes frontrun/backrun.
        None => MevKind::live_feed_default_kinds(),
    };
    let deadline = duration.map(parse_duration).transpose()?;

    let stop = stop_flag_with_deadline(deadline);
    let t0 = std::time::Instant::now();

    println!(
        "Explorer live feed — {chain} (tail of the indexed store; run `explorer index` alongside)"
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
        let total = store.op_count_since(0)?;
        if total > cursor {
            let take = (total - cursor).min(20) as usize;
            let feed = store.feed_tail(take, &kinds)?;
            for row in feed.iter().rev() {
                if row.profit_usd.unwrap_or(0.0) < min_profit_usd {
                    continue;
                }
                if let Some(shape) = arb_shape {
                    if !row_matches_arb_shape(row, shape) {
                        continue;
                    }
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

fn row_matches_arb_shape(row: &mev_scout_core::explorer::store::FeedRow, shape: &str) -> bool {
    if row.kind != "arb_atomic" && row.kind != "jit_arb" {
        return false;
    }
    let Some(details) = row.details_json.as_deref() else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(details) else {
        return false;
    };
    v.pointer("/arb_meta/arb_shape")
        .and_then(|x| x.as_str())
        .is_some_and(|s| s.eq_ignore_ascii_case(shape))
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
