//! ``explorer backfill`` — index a historical block range (clock or exact)
//! into the explorer store so the revenue-report windows (1d/7d/30d) have
//! realized data. Idempotent and gap-resumable via `blocks_classified`.

use super::*;

use crate::job_progress::BarProgress;

use mev_scout_core::chain::timing::blocks_per_day;
use mev_scout_core::explorer::ingest::{run_range, safe_head, IngestConfig};
use mev_scout_core::jobs::{init_rpc, load_pool_registry, BackfillOutcome};

pub async fn cmd_backfill(
    config: &Config,
    days: Option<u64>,
    from_block: Option<u64>,
    to_block: Option<u64>,
) -> anyhow::Result<BackfillOutcome> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let (_, chain_cfg) = validation::resolve_chain(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;
    cfg.arb_likely_parity = config.explorer.arb_likely_parity;

    let (from, to) = match (days, from_block, to_block) {
        (Some(d), None, None) => {
            let setup = init_rpc(config, chain, false).await?;
            let head = safe_head(&setup.rpc, &cfg).await?;
            let span = d.saturating_mul(blocks_per_day(chain));
            (head.saturating_sub(span), head)
        }
        (None, Some(f), Some(t)) => {
            if t < f {
                anyhow::bail!("--to-block ({t}) < --from-block ({f})");
            }
            (f, t)
        }
        (None, None, None) => anyhow::bail!("provide --days or --from-block with --to-block"),
        _ => anyhow::bail!("--days cannot be combined with --from-block/--to-block"),
    };
    if to < from {
        anyhow::bail!(
            "resolved range is empty (from {from} > to {to}) — the requested window is inside \
             the confirmation lag or past the chain tip"
        );
    }
    let span = (to - from + 1).max(1);
    anyhow::ensure!(
        span <= 100_000_000,
        "range is absurdly large ({span} blocks) — use --from-block/--to-block deliberately"
    );

    let setup = init_rpc(config, chain, true).await?;
    let store = explorer_store(config, chain)?;
    let pool_tokens = load_pool_registry(config, &chain);

    println!("Historical backfill — {chain} indexing {from}..={to} (~{span} blocks, resumable)");
    let t0 = std::time::Instant::now();
    let out = run_range(
        &setup.rpc,
        &store,
        &cfg,
        &pool_tokens,
        from,
        to,
        &BarProgress::new(),
    )
    .await?;
    let elapsed = t0.elapsed();
    println!(
        "Backfill done — {} new blocks indexed, {} ops in {:.1}s",
        out.blocks_processed,
        out.ops,
        elapsed.as_secs_f64()
    );
    Ok(BackfillOutcome {
        from_block: from,
        to_block: to,
        blocks_processed: out.blocks_processed,
        ops: out.ops,
        elapsed,
    })
}
