use anyhow::Context;
use std::time::{Duration, Instant};

use crate::cli::LiveArgs;
use crate::display::render_results_table;
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
    if let Some(format) = args.progress.as_deref() {
        if format != "json" {
            anyhow::bail!("unsupported --progress format '{format}' (only 'json')");
        }
    }
    if args.max_blocks.is_some() && !args.r#loop {
        anyhow::bail!("--max-blocks requires --loop");
    }
    let _ = deadline_from(args.r#loop, args.duration.as_deref(), Instant::now())?;

    let opts = LiveOpts {
        loop_enabled: args.r#loop,
        duration: args.duration.clone(),
        poll_interval_ms: args.poll_interval_ms,
        record_rejections: args.record_rejections,
        max_blocks: args.max_blocks,
    };

    match job_live(config, &opts, progress).await? {
        LiveOutcome::OneShot(pass) => {
            progress.log(&format!(
                "\nBlock {} — {} opportunity(ies) detected",
                pass.tip,
                pass.opportunities.len()
            ));
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
        LiveOutcome::Loop(_) => {}
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
