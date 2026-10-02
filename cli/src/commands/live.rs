use anyhow::Context;
use std::time::{Duration, Instant};

use crate::cli::LiveArgs;
use crate::display::{render_block_summary_table, render_results_table};
use crate::job_progress::JobProgress;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_live, LiveOpts, LiveOutcome};

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

pub async fn cmd_live(
    config: &Config,
    args: &LiveArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    if args.max_blocks.is_some() && !args.r#loop {
        anyhow::bail!("--max-blocks requires --loop");
    }
    let _ = deadline_from(args.r#loop, args.duration.as_deref(), Instant::now())?;

    // Ledger flags are validated in `resolve_ledger_policy` /
    // `resolve_native_price` so the rules live in one place and apply to any
    // caller, not just this CLI.
    let opts = LiveOpts {
        loop_enabled: args.r#loop,
        duration: args.duration.clone(),
        poll_interval_ms: config.live.poll_interval_ms,
        record_rejections: config.backtest.record_rejections,
        max_blocks: args.max_blocks,
        initial_balance_wei: args.initial_balance,
        initial_balance_usd: args.initial_balance_usd,
        reserve_wei: args.reserve,
        max_fills_per_block: args.max_fills_per_block,
        native_usd: args.native_usd,
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
            if !pass.block_stats.is_empty() {
                let s = &pass.block_stats[0];
                progress.log(&format!(
                    "  {} txs scanned, {} DEX, {} pending",
                    s.total_tx_count, s.dex_tx_count, s.pending_tx_count,
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
