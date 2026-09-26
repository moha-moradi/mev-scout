//! `mev-scout paper` — virtual-fund bot P&L.

use comfy_table::Table;

use crate::cli::{
    PaperArgs, PaperCommand, PaperLiveArgs, PaperRunArgs, PaperSimArgs, PaperStatsArgs,
};
use crate::job_progress::JobProgress;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{
    job_paper_live, job_paper_run, job_paper_sim, job_paper_stats, PaperLiveOpts, PaperRunOpts,
    PaperSimOpts,
};
use mev_scout_core::pipeline::aggregate_fills;
use mev_scout_core::utils::epoch_secs;

fn since_ts(since: Option<&str>) -> u64 {
    let now = epoch_secs();
    match since {
        Some("1d") => now.saturating_sub(86_400),
        Some("7d") => now.saturating_sub(7 * 86_400),
        Some("30d") => now.saturating_sub(30 * 86_400),
        Some("all") | None => 0,
        Some(other) => {
            tracing::warn!("unknown --since '{other}', using all");
            0
        }
    }
}

pub async fn cmd_paper(
    config: &Config,
    args: &PaperArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    match &args.command {
        PaperCommand::Run(a) => cmd_paper_run(config, a, progress).await,
        PaperCommand::Live(a) => cmd_paper_live(config, a, progress).await,
        PaperCommand::Sim(a) => cmd_paper_sim(config, a, progress).await,
        PaperCommand::Stats(a) => cmd_paper_stats(config, a, progress).await,
    }
}

async fn cmd_paper_run(
    config: &Config,
    _args: &PaperRunArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let opts = PaperRunOpts {
        batch_rpc: config.backtest.batch_rpc,
        record_rejections: config.backtest.record_rejections,
    };
    let _ = job_paper_run(config, &opts, progress).await?;
    Ok(())
}

async fn cmd_paper_live(
    config: &Config,
    args: &PaperLiveArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    if args.max_blocks.is_some() && !args.r#loop {
        anyhow::bail!("--max-blocks requires --loop");
    }
    if args.duration.is_some() && !args.r#loop {
        anyhow::bail!("--duration requires --loop");
    }
    let opts = PaperLiveOpts {
        loop_enabled: args.r#loop,
        duration: args.duration.clone(),
        poll_interval_ms: config.live.poll_interval_ms,
        record_rejections: config.backtest.record_rejections,
        max_blocks: args.max_blocks,
    };
    let _ = job_paper_live(config, &opts, progress).await?;
    Ok(())
}

async fn cmd_paper_sim(
    config: &Config,
    args: &PaperSimArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let opts = PaperSimOpts {
        run_id: args.run_id.clone(),
        wallet_multiplier: args.wallet_multiplier,
    };
    let _ = job_paper_sim(config, &opts, progress).await?;
    Ok(())
}

async fn cmd_paper_stats(
    config: &Config,
    args: &PaperStatsArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let _ = progress;
    let outcome = job_paper_stats(
        config,
        args.session.as_deref(),
        since_ts(args.since.as_deref()),
        50,
    )?;

    println!("PAPER (theoretical, no competition)");
    if outcome.sessions.is_empty() {
        println!("  (no paper sessions)");
        return Ok(());
    }

    let mut table = Table::new();
    table.set_header(vec![
        "session",
        "mode",
        "blocks",
        "fills",
        "skipped",
        "net wei",
        "drawdown",
        "linked run",
    ]);
    for s in &outcome.sessions {
        table.add_row(vec![
            s.session_id.clone(),
            s.mode.clone(),
            format!("{}-{}", s.start_block, s.end_block),
            s.fills.to_string(),
            s.skipped.to_string(),
            s.net_profit_wei.clone(),
            s.max_drawdown_wei.clone(),
            s.linked_run_id.clone().unwrap_or_else(|| "-".into()),
        ]);
    }
    println!("{table}");

    if !outcome.fills.is_empty() {
        println!("\nFills:");
        let mut ft = Table::new();
        ft.set_header(vec![
            "block",
            "tx",
            "strategy",
            "gross",
            "gas",
            "net",
            "wallet after",
        ]);
        for f in &outcome.fills {
            ft.add_row(vec![
                f.block_number.to_string(),
                f.tx_index
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
                f.strategy.clone(),
                f.gross_wei.to_string(),
                f.gas_wei.to_string(),
                f.net_wei.to_string(),
                f.wallet_after.to_string(),
            ]);
        }
        println!("{ft}");

        // Aggregated P&L / ROI rollup over the session's fills (MEV-VERIFICATION
        // §C.3): the same `aggregate` shapes `report` uses, fed from the
        // persisted paper fills. `paper stats` is offline, so no price snapshot
        // is available — pass 0.0 and the USD fields read zero by construction
        // rather than guessing a rate.
        let agg = aggregate_fills(&outcome.fills, 0.0);
        let s = &agg.summary;
        println!();
        println!("  ── Aggregated P&L ──");
        println!(
            "  fills {} ({} profitable) | gross {:.4} ETH | gas {:.4} ETH | net {:.4} ETH",
            s.total, s.profitable, s.gross_revenue, s.total_cost, s.net_profit,
        );
        if let Some(best) = &s.best_strategy {
            println!("  best strategy: {best}");
        }
        println!("  strategy       fills     net (ETH)     ROI%  best (ETH)");
        let mut strat: Vec<_> = agg.by_strategy.values().collect();
        strat.sort_by(|a, b| {
            b.net_profit
                .partial_cmp(&a.net_profit)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for m in strat {
            println!(
                "  {:<12} {:>6} {:>13.4} {:>7.1} {:>11.4}",
                m.strategy, m.count, m.net_profit, m.roi, m.best_opp,
            );
        }
    }
    Ok(())
}
