//! Historical explorer backfill: index a closed block range (clock or exact)
//! into the explorer store so the revenue report's 1d/7d/30d windows have
//! realized data. Idempotent and gap-resumable via `blocks_classified`.

use std::time::{Duration, Instant};

use anyhow::{bail, Context};

use crate::chain::timing::blocks_per_day;
use crate::config::validation;
use crate::config::Config;
use crate::explorer::ingest::{run_range, safe_head, IngestConfig};
use crate::explorer::store::ExplorerStore;
use crate::progress::JobProgress;
use crate::types::ChainName;

use super::index::load_pool_registry;
use super::rpc::init_rpc;

#[derive(Debug, Clone, Default)]
pub struct BackfillOpts {
    /// Backfill the trailing N days up to the confirmed tip.
    pub days: Option<u64>,
    /// Exact inclusive range start (with `to_block`).
    pub from_block: Option<u64>,
    /// Exact inclusive range end (with `from_block`).
    pub to_block: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct BackfillOutcome {
    pub from_block: u64,
    pub to_block: u64,
    pub blocks_processed: u64,
    pub ops: u64,
    pub elapsed: Duration,
}

pub async fn job_backfill(
    config: &Config,
    opts: &BackfillOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<BackfillOutcome> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let setup = init_rpc(config, chain, true).await?;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;

    let (_, chain_cfg) = validation::resolve_chain(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;
    cfg.arb_likely_parity = config.explorer.arb_likely_parity;

    let pool_tokens = load_pool_registry(config, &chain);

    let (from, to) = resolve_range(&setup, &cfg, chain, opts).await?;
    if to < from {
        bail!(
            "resolved range is empty (from {from} > to {to}) — the window is inside the \
             confirmation lag or past the chain tip"
        );
    }
    progress.log(&format!(
        "Historical backfill — {chain} indexing {from}..={to} (~{} blocks, resumable)",
        to - from + 1
    ));

    let t0 = Instant::now();
    let out = run_range(&setup.rpc, &store, &cfg, &pool_tokens, from, to, progress).await?;
    let elapsed = t0.elapsed();
    progress.log(&format!(
        "Backfill done — {} new blocks indexed, {} ops in {:.1}s",
        out.blocks_processed,
        out.ops,
        elapsed.as_secs_f64()
    ));
    Ok(BackfillOutcome {
        from_block: from,
        to_block: to,
        blocks_processed: out.blocks_processed,
        ops: out.ops,
        elapsed,
    })
}

/// Resolve `--days` or `--from-block/--to-block` into an inclusive range.
/// `run_range` re-clamps the end to `head − confirmations`; the `--days` path
/// already aims at the confirmed tip via `safe_head`.
async fn resolve_range(
    setup: &super::rpc::RpcSetup,
    cfg: &IngestConfig,
    chain: ChainName,
    opts: &BackfillOpts,
) -> anyhow::Result<(u64, u64)> {
    match (opts.days, opts.from_block, opts.to_block) {
        (Some(days), None, None) => {
            if days == 0 {
                bail!("--days must be >= 1");
            }
            let head = safe_head(&setup.rpc, cfg)
                .await
                .context("resolve safe head for --days")?;
            let span = days.saturating_mul(blocks_per_day(chain));
            let from = head.saturating_sub(span);
            Ok((from, head))
        }
        (None, Some(f), Some(t)) => {
            if t < f {
                bail!("--to-block ({t}) < --from-block ({f})");
            }
            Ok((f, t))
        }
        (None, None, None) => bail!("provide --days or --from-block with --to-block"),
        _ => bail!("--days cannot be combined with --from-block/--to-block"),
    }
}
