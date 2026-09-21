//! Paper jobs: detect (or replay) opportunities, apply ledger, persist session.

use anyhow::Context;

use crate::config::validation;
use crate::config::Config;
use crate::explorer::store::ExplorerStore;
use crate::jobs::{job_live, job_run, LiveOpts, LiveOutcome, RunOpts};
use crate::paper::types::{LedgerResult, PaperFill, PaperMode, PaperSession};
use crate::paper::LedgerPolicy;
use crate::progress::JobProgress;
use crate::types::{ChainName, MevOpportunity};
use crate::utils::epoch_secs;

#[derive(Debug, Clone)]
pub struct PaperRunOpts {
    pub batch_rpc: bool,
    pub record_rejections: bool,
}

#[derive(Debug, Clone)]
pub struct PaperLiveOpts {
    pub loop_enabled: bool,
    pub duration: Option<String>,
    pub poll_interval_ms: u64,
    pub record_rejections: bool,
    pub max_blocks: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PaperSimOpts {
    pub run_id: String,
    /// Multiplier applied to `paper.starting_gas_wei` (e.g. 2.0 = 2× wallet).
    pub wallet_multiplier: f64,
}

pub struct PaperOutcome {
    pub session_id: String,
    pub linked_run_id: Option<String>,
    pub mode: PaperMode,
    pub ledger: LedgerResult,
    pub opportunity_count: usize,
}

pub struct PaperStatsOutcome {
    pub sessions: Vec<PaperSession>,
    pub fills: Vec<PaperFill>,
}

fn policy_from_config(config: &Config) -> anyhow::Result<LedgerPolicy> {
    config.paper.ledger_policy()
}

fn persist_session_for_chain(
    config: &Config,
    chain: ChainName,
    mode: PaperMode,
    linked_run_id: Option<&str>,
    ledger: &LedgerResult,
) -> anyhow::Result<String> {
    let session_id = format!("paper_{}_{}", mode.as_str(), epoch_secs());
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;
    store.insert_paper_session(
        &session_id,
        &chain.to_string(),
        mode,
        linked_run_id,
        ledger,
    )?;
    Ok(session_id)
}

fn render_summary(progress: &dyn JobProgress, outcome: &PaperOutcome) {
    let l = &outcome.ledger;
    progress.log("PAPER (theoretical, no competition)");
    progress.log(&format!("  session:     {}", outcome.session_id));
    if let Some(rid) = &outcome.linked_run_id {
        progress.log(&format!("  linked run:  {rid}"));
    }
    progress.log(&format!("  mode:        {}", outcome.mode));
    progress.log(&format!(
        "  opportunities scanned: {}",
        outcome.opportunity_count
    ));
    progress.log(&format!(
        "  fills: {} | skipped: {}",
        l.fills_count(),
        l.skips_count()
    ));
    progress.log(&format!(
        "  wallet: {} → {} wei (reserve {})",
        l.starting_gas_wei, l.ending_gas_wei, l.reserve_wei
    ));
    progress.log(&format!(
        "  gross: {} | gas: {} | net: {} | best fill: {} | max drawdown: {}",
        l.gross_wei, l.gas_wei, l.net_profit_wei, l.best_net_wei, l.max_drawdown_wei
    ));
}

/// Historical paper session: `job_run` then ledger.
pub async fn job_paper_run(
    config: &Config,
    opts: &PaperRunOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<PaperOutcome> {
    let run_opts = RunOpts {
        batch_rpc: opts.batch_rpc,
        record_rejections: opts.record_rejections,
    };
    let run = job_run(config, &run_opts, progress).await?;
    let (chain, _) = validation::resolve_chain(config).context("resolve chain")?;
    let policy = policy_from_config(config)?;
    let ledger = policy.apply(&run.opportunities);
    let session_id = persist_session_for_chain(
        config,
        chain,
        PaperMode::Run,
        Some(&run.run_id),
        &ledger,
    )?;
    let outcome = PaperOutcome {
        session_id,
        linked_run_id: Some(run.run_id),
        mode: PaperMode::Run,
        ledger,
        opportunity_count: run.opportunities.len(),
    };
    render_summary(progress, &outcome);
    Ok(outcome)
}

/// Live paper session. One-shot uses `job_live` once; `--loop` runs repeated
/// one-shot passes (because `LiveOutcome::Loop` drops per-pass opps) and
/// aggregates into one cumulative ledger session.
pub async fn job_paper_live(
    config: &Config,
    opts: &PaperLiveOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<PaperOutcome> {
    let (chain, _) = validation::resolve_chain(config).context("resolve chain")?;
    let policy = policy_from_config(config)?;

    if !opts.loop_enabled {
        let live_opts = LiveOpts {
            loop_enabled: false,
            duration: None,
            poll_interval_ms: opts.poll_interval_ms,
            record_rejections: opts.record_rejections,
            max_blocks: None,
        };
        let live = job_live(config, &live_opts, progress).await?;
        let (run_id, opps) = match live {
            LiveOutcome::OneShot(pass) => (pass.run_id, pass.opportunities),
            LiveOutcome::Loop(_) => anyhow::bail!("internal: expected one-shot live outcome"),
        };
        let ledger = policy.apply(&opps);
        let session_id =
            persist_session_for_chain(config, chain, PaperMode::Live, Some(&run_id), &ledger)?;
        let outcome = PaperOutcome {
            session_id,
            linked_run_id: Some(run_id),
            mode: PaperMode::Live,
            ledger,
            opportunity_count: opps.len(),
        };
        render_summary(progress, &outcome);
        return Ok(outcome);
    }

    let deadline = match opts.duration.as_deref() {
        Some(d) => {
            let dur = humantime::parse_duration(d)
                .with_context(|| format!("invalid --duration '{d}'"))?;
            Some(std::time::Instant::now() + dur)
        }
        None => None,
    };

    let mut all_opps: Vec<MevOpportunity> = Vec::new();
    let mut linked_ids: Vec<String> = Vec::new();
    let mut blocks_processed: u64 = 0;
    let mut last_tip: Option<u64> = None;

    loop {
        if progress.cancelled() {
            break;
        }
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                break;
            }
        }
        if let Some(mb) = opts.max_blocks {
            if blocks_processed >= mb {
                break;
            }
        }

        let live_opts = LiveOpts {
            loop_enabled: false,
            duration: None,
            poll_interval_ms: opts.poll_interval_ms,
            record_rejections: opts.record_rejections,
            max_blocks: None,
        };
        let live = job_live(config, &live_opts, progress).await?;
        let (run_id, tip, opps) = match live {
            LiveOutcome::OneShot(pass) => (pass.run_id, pass.tip, pass.opportunities),
            LiveOutcome::Loop(_) => anyhow::bail!("internal: expected one-shot live outcome"),
        };

        // Skip duplicate tip if chain has not advanced since last pass.
        if last_tip == Some(tip) {
            tokio::time::sleep(std::time::Duration::from_millis(opts.poll_interval_ms)).await;
            continue;
        }
        last_tip = Some(tip);
        linked_ids.push(run_id);
        let n = opps.len();
        all_opps.extend(opps);
        blocks_processed = blocks_processed.saturating_add(1);
        progress.log(&format!(
            "paper live pass tip={tip} opps={n} cumulative={}",
            all_opps.len()
        ));

        tokio::time::sleep(std::time::Duration::from_millis(opts.poll_interval_ms)).await;
    }

    let ledger = policy.apply(&all_opps);
    let linked = if linked_ids.is_empty() {
        None
    } else {
        Some(linked_ids.join(","))
    };
    let session_id = persist_session_for_chain(
        config,
        chain,
        PaperMode::Live,
        linked.as_deref(),
        &ledger,
    )?;
    let outcome = PaperOutcome {
        session_id,
        linked_run_id: linked,
        mode: PaperMode::Live,
        ledger,
        opportunity_count: all_opps.len(),
    };
    render_summary(progress, &outcome);
    Ok(outcome)
}

/// Offline replay: load opportunities for `run_id`, apply ledger (optional wallet multiplier).
pub async fn job_paper_sim(
    config: &Config,
    opts: &PaperSimOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<PaperOutcome> {
    let (chain, _) = validation::resolve_chain(config).context("resolve chain")?;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;
    let opps = store
        .opportunities_by_run(&opts.run_id)
        .with_context(|| format!("load opportunities for run {}", opts.run_id))?;
    if opps.is_empty() {
        anyhow::bail!(
            "no opportunities for run_id '{}' — run `mev-scout run`/`live` first",
            opts.run_id
        );
    }

    let mut policy = policy_from_config(config)?;
    if (opts.wallet_multiplier - 1.0).abs() > f64::EPSILON {
        if opts.wallet_multiplier <= 0.0 || !opts.wallet_multiplier.is_finite() {
            anyhow::bail!("invalid --wallet-multiplier {}", opts.wallet_multiplier);
        }
        let base = policy.starting_gas_wei as f64 * opts.wallet_multiplier;
        if !base.is_finite() || base < 0.0 {
            anyhow::bail!("wallet multiplier overflow");
        }
        policy.starting_gas_wei = base.min(u128::MAX as f64) as u128;
    }

    progress.log(&format!(
        "PAPER sim — replaying {} opps from {}",
        opps.len(),
        opts.run_id
    ));
    let ledger = policy.apply(&opps);
    let session_id = persist_session_for_chain(
        config,
        chain,
        PaperMode::Sim,
        Some(&opts.run_id),
        &ledger,
    )?;
    let outcome = PaperOutcome {
        session_id,
        linked_run_id: Some(opts.run_id.clone()),
        mode: PaperMode::Sim,
        ledger,
        opportunity_count: opps.len(),
    };
    render_summary(progress, &outcome);
    Ok(outcome)
}

/// List / show paper session stats (offline).
pub fn job_paper_stats(
    config: &Config,
    session_id: Option<&str>,
    since_ts: u64,
    limit: usize,
) -> anyhow::Result<PaperStatsOutcome> {
    let (chain, _) = validation::resolve_chain(config)?;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;
    if let Some(id) = session_id {
        let session = store
            .paper_session(id)?
            .ok_or_else(|| anyhow::anyhow!("paper session '{id}' not found"))?;
        let fills = store.paper_fills(id)?;
        Ok(PaperStatsOutcome {
            sessions: vec![session],
            fills,
        })
    } else {
        let sessions = store.list_paper_sessions(since_ts, limit)?;
        Ok(PaperStatsOutcome {
            sessions,
            fills: Vec::new(),
        })
    }
}
