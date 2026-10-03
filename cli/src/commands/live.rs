use anyhow::Context;
use std::time::{Duration, Instant};

use crate::cli::LiveArgs;
use crate::display::{render_block_summary_table, render_results_table};
use crate::job_progress::JobProgress;
use mev_scout_core::cache::{SqliteStore, TokenCache};
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_discover, job_live, DiscoverOpts, LiveOpts, LiveOutcome};

pub fn parse_duration_str(s: &str) -> anyhow::Result<Duration> {
    humantime::parse_duration(s)
        .with_context(|| format!("invalid --duration '{s}' (expected e.g. 90s, 15m, 1h)"))
}

pub fn deadline_from(
    loop_enabled: bool,
    duration: Option<&str>,
    now: Instant,
) -> anyhow::Result<Option<Instant>> {
    match duration {
        Some(d) => {
            if !loop_enabled {
                anyhow::bail!("--duration requires --loop");
            }
            Ok(Some(now + parse_duration_str(d)?))
        }
        None => Ok(None),
    }
}

/// Open the SQLite cache for the configured chain, or `None` when it cannot be
/// opened — a bootstrap is an optimization, so an unreadable cache must not
/// abort the run that was asked for.
fn open_cache(config: &Config) -> Option<SqliteStore> {
    let (chain, _) = mev_scout_core::config::validation::resolve_chain(config).ok()?;
    let path = config.effective_db_path(&chain);
    match SqliteStore::open(&path) {
        Ok(store) => Some(store),
        Err(e) => {
            tracing::warn!("bootstrap: cannot open cache {}: {e:#}", path);
            None
        }
    }
}

/// Populate the token cache from the bundled known-token list when it is empty.
/// Fully offline: `known_tokens.json` ships with the binary.
async fn bootstrap_tokens(config: &Config, cache: &SqliteStore, progress: &dyn JobProgress) {
    let persisted = match TokenCache::load(cache) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("bootstrap: token cache load failed: {e:#}");
            return;
        }
    };
    if !persisted.is_empty() {
        return;
    }

    let chain = match mev_scout_core::config::validation::resolve_chain(config) {
        Ok((chain, _)) => chain,
        Err(e) => {
            tracing::debug!("bootstrap: chain resolution failed: {e:#}");
            return;
        }
    };

    let warmed = TokenCache::warm(chain.chain_id());
    match warmed.persist_all(cache) {
        Ok(saved) => progress.log(&format!(
            "Bootstrap: seeded {saved} token(s) from known list"
        )),
        Err(e) => tracing::warn!("bootstrap: token cache persist failed: {e:#}"),
    }
}

/// Run pool discovery when the cache is empty, so `live` is usable on a fresh
/// clone with no explicit `discover` step.
async fn bootstrap_pools(config: &Config, cache: &SqliteStore, progress: &dyn JobProgress) {
    // Only an *empty* cache triggers a rescan. A higher threshold would re-run
    // a full discovery on every invocation whenever the user deliberately
    // backfilled a narrow range, which is slower than the small pool set is
    // worth.
    if cache.pool_count().unwrap_or(0) > 0 {
        return;
    }

    progress.log("Bootstrap: no pools cached, running pool discovery...");
    let d = &config.discover;
    let opts = DiscoverOpts {
        source: "onchain".to_string(),
        enrich: false,
        min_tvl: if d.min_tvl > 0.0 {
            Some(d.min_tvl)
        } else {
            None
        },
        max_pools: d.max_pools,
        batch_size: d.batch_size,
        rpc_concurrency: d.rpc_concurrency,
        // A fresh cache has nothing to resume from; the window falls back to
        // the chain's configured lookback.
        incremental: false,
        health_check: d.health_check,
        json: matches!(
            config.output.output,
            mev_scout_core::types::OutputFormat::Json
        ),
        solidly_fee_bps: d.solidly_fee_bps.map(u64::from),
        resolve_remote_metadata: false,
    };

    match job_discover(config, &opts, progress).await {
        Ok(outcome) => progress.log(&format!(
            "Bootstrap: cached {} pool(s)",
            cache.pool_count().unwrap_or(outcome.pools_found)
        )),
        // Discovery needs RPC and factory logs; if that fails, `live` will
        // report the same failure with more context.
        Err(e) => tracing::warn!("bootstrap: pool discovery failed: {e:#}"),
    }
}

/// Ensure the caches `live` depends on are populated. Failures are logged, not
/// fatal — the scan itself is still worth attempting and produces the actionable
/// error if the chain is unreachable.
async fn bootstrap(config: &Config, progress: &dyn JobProgress) {
    let Some(cache) = open_cache(config) else {
        return;
    };
    bootstrap_tokens(config, &cache, progress).await;
    bootstrap_pools(config, &cache, progress).await;
}

pub async fn cmd_live(
    config: &Config,
    args: &LiveArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    if args.max_blocks.is_some() && !args.r#loop {
        anyhow::bail!("--max-blocks requires --loop");
    }
    let _ = deadline_from(args.r#loop, args.duration.as_deref(), Instant::now())?;

    // Validate the config *before* bootstrapping. Bootstrap issues a full
    // on-chain discovery against the chain's public RPCs, so running it ahead
    // of validation turns a typo'd chain or an unusable gas config into minutes
    // of network work followed by a confusing downstream error. `validate_live`
    // is a pure config invariant check (offline), so this is the cheap way to
    // fail loudly and locally.
    mev_scout_core::config::validation::validate_live(config).context("invalid configuration")?;

    bootstrap(config, progress).await;

    // Ledger flags are validated in `resolve_ledger_policy` /
    // `resolve_native_price` so the rules live in one place and apply to any
    // caller, not just this CLI.
    let opts = LiveOpts {
        loop_enabled: args.r#loop,
        duration: args.duration.clone(),
        poll_interval_ms: config.live.poll_interval_ms,
        record_rejections: config.backtest.record_rejections,
        max_blocks: args.max_blocks,
        single_pass_blocks: args.blocks,
        initial_balance_wei: args.initial_balance,
        initial_balance_usd: None,
        reserve_wei: args.reserve,
        max_fills_per_block: None,
        native_usd: None,
    };

    match job_live(config, &opts, progress).await? {
        LiveOutcome::OneShot(pass) => {
            // The "Block N — … detected" headline is already emitted by
            // `job_live` (core), together with the ledger summary. Only the
            // rendering and the scan counters are left to the CLI here.
            if pass.opportunities.is_empty() {
                progress.log("No MEV opportunities in this block.");
            } else {
                render_results_table(&pass.opportunities, None);
            }
            // Every block in the pass reports its own counters; a single-block
            // window still shows one line, a wider one sums them.
            let txs: usize = pass.block_stats.iter().map(|s| s.total_tx_count).sum();
            let dex: usize = pass.block_stats.iter().map(|s| s.dex_tx_count).sum();
            let pending: usize = pass.block_stats.iter().map(|s| s.pending_tx_count).sum();
            if !pass.block_stats.is_empty() {
                progress.log(&format!(
                    "  {} block(s) scanned, {txs} txs, {dex} DEX, {pending} pending",
                    pass.block_stats.len()
                ));
            }
        }
        LiveOutcome::Loop(session) => {
            if session.block_stats.len() > 1 {
                render_block_summary_table(&session.block_stats);
            }
            // Ledger totals were already logged per pass plus once at session
            // end; only the session id is echoed here for copy-paste into
            // `report` / `explorer`.
            progress.log(&format!("\nPaper session: {}", session.session_id));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_humantime_suffixes() {
        assert_eq!(parse_duration_str("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(
            parse_duration_str("15m").unwrap(),
            Duration::from_secs(15 * 60)
        );
    }
}
