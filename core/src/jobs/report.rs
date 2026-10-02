use anyhow::Context;

use crate::cache::{RunManifest, SqliteStore};
use crate::config::validation;
use crate::config::Config;
use crate::explorer::store::ExplorerStore;
use crate::paper::PaperSession;
use crate::progress::JobProgress;
use crate::types::MevOpportunity;

#[derive(Debug, Clone, Default)]
pub struct ReportOpts {
    pub run_id: Option<String>,
}

pub struct ReportOutcome {
    pub manifest: RunManifest,
    pub opportunities: Vec<MevOpportunity>,
    /// Ledger session attached to this run, when one exists. `live` always
    /// writes one; runs from before the ledger was always-on only have one if
    /// the (now-removed) `paper run` was used.
    pub ledger_session: Option<PaperSession>,
}

pub async fn job_report(
    config: &Config,
    opts: &ReportOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<ReportOutcome> {
    let (chain_name, _) = validation::resolve_chain(config).context("invalid configuration")?;

    let cache = SqliteStore::open(config.effective_db_path(&chain_name))
        .with_context(|| "failed to open run-history db")?;

    let run_id = match &opts.run_id {
        Some(id) => id.clone(),
        None => {
            let latest = cache.latest_manifest()?.context(
                "no runs recorded in the run-history db — execute 'mev-scout live' first",
            )?;
            latest.run_id
        }
    };

    let manifest = cache
        .get_manifest(&run_id)?
        .with_context(|| format!("run '{run_id}' not found"))?;

    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain_name))?;
    let opportunities = store.opportunities_by_run(&run_id)?;

    progress.log(&format!("Run ID:        {}", manifest.run_id));
    progress.log(&format!("Chain:         {}", manifest.chain));
    progress.log(&format!(
        "Block range:   {}–{}",
        manifest.start_block, manifest.end_block
    ));
    progress.log(&format!("Mode:          {}", manifest.range_mode));
    progress.log(&format!("Opportunities: {}", opportunities.len()));

    // `linked_run_id` is what ties the ledger session back to the run, so a
    // `live` session shows its P&L here without a separate `paper stats` call.
    let ledger_session = store
        .paper_session_for_run(&run_id)
        .with_context(|| format!("ledger lookup failed for run '{run_id}'"))?;

    if let Some(session) = &ledger_session {
        progress.log(&format!("Ledger:        {}", session.session_id));
        progress.log(&format!(
            "  fills: {} | skipped: {} | net profit: {} wei | max drawdown: {} wei",
            session.fills, session.skipped, session.net_profit_wei, session.max_drawdown_wei,
        ));
        progress.log(&format!(
            "  wallet: {} → {} wei (reserve {})",
            session.starting_gas_wei, session.ending_gas_wei, session.reserve_wei
        ));
    }

    Ok(ReportOutcome {
        manifest,
        opportunities,
        ledger_session,
    })
}
