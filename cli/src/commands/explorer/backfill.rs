//! ``explorer index`` — index a historical block range (clock or exact) into
//! the explorer store, then emit the range's dashboard.
//! Idempotent and gap-resumable via `blocks_classified`.
use super::*;

use crate::job_progress::JobProgress;

use mev_scout_core::chain::timing::blocks_per_day;
use mev_scout_core::config::{validation, Config};
use mev_scout_core::explorer::ingest::{
    run_range, safe_head, IngestConfig, PoolViews, RangeRequest,
};
use mev_scout_core::jobs::{init_rpc, load_pool_registry, BackfillOutcome};

const DEFAULT_BACKFILL_WINDOW: &str = "7d";
const MAX_BACKFILL_SECS: u64 = 365 * 86_400;
const SECS_PER_DAY: u64 = 86_400;

#[allow(clippy::too_many_arguments)]
pub async fn cmd_backfill(
    config: &Config,
    window: Option<&str>,
    from_block: Option<u64>,
    to_block: Option<u64>,
    reclassify: bool,
    discover_pools: bool,
    progress: &dyn JobProgress,
) -> anyhow::Result<BackfillOutcome> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let (_, chain_cfg) = validation::resolve_chain(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cfg = IngestConfig::from_app_config(chain, &chain_cfg, config);

    let trailing_secs = match (window, from_block, to_block) {
        (Some(w), None, None) => Some(parse_backfill_duration(w)?),
        (None, Some(f), Some(t)) => {
            if t < f {
                anyhow::bail!("--to-block ({t}) < --from-block ({f})");
            }
            None
        }
        (None, None, None) => Some(parse_backfill_duration(DEFAULT_BACKFILL_WINDOW)?),
        _ => anyhow::bail!(
            "--window and --from-block/--to-block are mutually exclusive — pick one range form"
        ),
    };

    let (from, to) = match trailing_secs {
        Some(secs) => {
            let setup = init_rpc(config, chain, false).await?;
            let head = safe_head(&setup.rpc, &cfg).await?;
            let span = window_span_blocks(chain, secs);
            (head.saturating_sub(span), head)
        }
        None => match (from_block, to_block) {
            (Some(f), Some(t)) => (f, t),
            _ => anyhow::bail!("--from-block and --to-block must be supplied together"),
        },
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

    if discover_pools {
        // Scope discovery to the same range: scanning the chain's full
        // lookback before a 1h/24h index is disproportionate, while active
        // pools in the range still populate token pairs and venue tags.
        progress.log(&format!(
            "Pool discovery — refreshing the scanner cache for {from}..={to} before indexing {chain}"
        ));
        let mut dargs = crate::cli::DiscoverArgs::default();
        dargs.block_range.from_block = Some(from);
        dargs.block_range.to_block = Some(to);
        crate::commands::cmd_discover(config, &dargs, progress).await?;
    }

    let setup = init_rpc(config, chain, true).await?;
    let store = explorer_store(config, chain)?;
    if reclassify {
        store.purge_range(from, to)?;
        progress.log(&format!(
            "Reclassify — purged derived facts for {from}..={to} (JIT journals replayed)"
        ));
    }
    let registry = load_pool_registry(config, &chain);
    let bc = config.effective_block_concurrency(chain, &setup.provider_configs);
    progress.log(&format!(
        "Registry — {} token pairs, {} v2-like (skim), {} epoch venues",
        registry.tokens.len(),
        registry.v2_like.len(),
        registry.epoch_venue.len()
    ));
    if registry.v2_like.is_empty() {
        let unverified = registry.health.unverified_v2_like;
        progress.log(&format!(
            "WARNING: no skim-eligible V2 pools — {unverified} V2-family row(s) lack a verified \
             factory; skim detection and epoch venue tags are disabled. On-chain discovery over \
             pool creation blocks is required (`--discover-pools` only scans the index window); \
             re-index with `--reclassify` afterwards."
        ));
    }

    progress.log(&format!(
        "Historical backfill — {chain} indexing {from}..={to} (~{span} blocks, resumable, concurrency={bc})"
    ));
    let t0 = std::time::Instant::now();
    let out = run_range(
        &setup.rpc,
        &store,
        &cfg,
        PoolViews {
            tokens: &registry.tokens,
            v2_like: &registry.v2_like,
            epoch_venue: &registry.epoch_venue,
            fee_bps: &registry.fee_bps,
            dex_name: &registry.dex_name,
        },
        RangeRequest {
            from_block: from,
            to_block: to,
            block_concurrency: bc,
        },
        progress,
    )
    .await?;
    let elapsed = t0.elapsed();
    progress.log(&format!(
        "Backfill done — {} new blocks indexed, {} ops in {:.1}s",
        out.blocks_processed,
        out.ops,
        elapsed.as_secs_f64()
    ));
    if !out.skipped_blocks.is_empty() {
        const SHOW: usize = 20;
        let n = out.skipped_blocks.len();
        let shown = out
            .skipped_blocks
            .iter()
            .take(SHOW)
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let more = if n > SHOW { "…" } else { "" };
        tracing::warn!(
            skipped = n,
            "backfill skipped {n} blocks whose fetch failed after retries \
             [{shown}{more}]; re-run the same range to fill the gap"
        );
    }
    Ok(BackfillOutcome {
        from_block: from,
        to_block: to,
        blocks_processed: out.blocks_processed,
        ops: out.ops,
        elapsed,
    })
}

fn parse_backfill_duration(s: &str) -> anyhow::Result<u64> {
    let secs = parse_duration_secs(s).ok_or_else(|| {
        anyhow::anyhow!("invalid --window '{s}' (expected a positive Nh, Nd, or Nw duration)")
    })?;
    anyhow::ensure!(
        secs <= MAX_BACKFILL_SECS,
        "--window '{s}' exceeds the 365-day limit"
    );
    Ok(secs)
}

/// `Nh` / `Nd` / `Nw`. Zero and unknown suffixes are rejected.
fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    if num.is_empty() {
        return None;
    }
    let v: u64 = num.parse().ok()?;
    if v == 0 {
        return None;
    }
    let factor = match unit {
        "h" => 3_600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return None,
    };
    Some(v.saturating_mul(factor))
}

/// Use the authoritative blocks/day table so `24h == 1d` on every chain,
/// including sub-second chains where rounded block time would distort spans.
fn window_span_blocks(chain: ChainName, secs: u64) -> u64 {
    secs.saturating_mul(blocks_per_day(chain))
        .saturating_div(SECS_PER_DAY)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_suffixes() {
        assert_eq!(parse_backfill_duration("12h").unwrap(), 12 * 3_600);
        assert_eq!(parse_backfill_duration("1d").unwrap(), 86_400);
        assert_eq!(parse_backfill_duration("2w").unwrap(), 14 * 86_400);
        assert_eq!(
            parse_backfill_duration("24h").unwrap(),
            parse_backfill_duration("1d").unwrap()
        );
    }

    #[test]
    fn duration_bounds_and_bad_input_are_rejected() {
        assert!(parse_backfill_duration("365d").is_ok());
        assert!(parse_backfill_duration("52w").is_ok());
        assert!(parse_backfill_duration("366d").is_err());
        assert!(parse_backfill_duration("53w").is_err());
        assert!(parse_backfill_duration("0d").is_err());
        assert!(parse_backfill_duration("1x").is_err());
        assert!(parse_backfill_duration("d").is_err());
        assert!(parse_backfill_duration("all").is_err());
    }

    #[test]
    fn span_uses_authoritative_blocks_per_day() {
        let arbitrum = ChainName::Arbitrum;
        let per_day = blocks_per_day(arbitrum);
        assert_eq!(window_span_blocks(arbitrum, 86_400), per_day);
        assert_eq!(window_span_blocks(arbitrum, 3_600), per_day / 24);
        assert_eq!(window_span_blocks(arbitrum, 14 * 86_400), per_day * 14);
        assert!(
            window_span_blocks(arbitrum, 86_400) > 86_400,
            "rounded block-time conversion would undercount Arbitrum"
        );
    }
}
