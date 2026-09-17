//! Cross-validation job: realized explorer ops vs scanner opportunities.

use serde::Serialize;

use crate::config::validation;
use crate::config::Config;
use crate::explorer::store::ExplorerStore;
use crate::explorer::validate::{self, ValidationReport};
use crate::progress::JobProgress;
use crate::utils::epoch_secs;

#[derive(Debug, Clone, Default)]
pub struct ExplorerValidateOpts {
    pub since: Option<String>,
    pub match_window: u64,
    pub run_ids: Vec<String>,
    pub threshold_sweep: bool,
    pub emit_missing_pools: bool,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExplorerValidateOutcome {
    pub report: ValidationReport,
    pub missing_pools_path: Option<String>,
}

fn since_ts(since: Option<&str>) -> u64 {
    let now = epoch_secs();
    match since {
        Some("1d") => now - 86_400,
        Some("7d") => now - 7 * 86_400,
        Some("30d") => now - 30 * 86_400,
        Some("all") | None => 0,
        Some(other) => {
            tracing::warn!("unknown --since '{other}', using all");
            0
        }
    }
}

fn block_window(store: &ExplorerStore, since_ts: u64) -> anyhow::Result<(u64, u64)> {
    let ops = store.ops_since(since_ts)?;
    let mut blocks: Vec<u64> = ops.iter().map(|o| o.block_number).collect();
    if blocks.is_empty() {
        anyhow::bail!("no indexed ops in the requested window — run explorer index first");
    }
    blocks.sort_unstable();
    Ok((blocks[0], blocks[blocks.len() - 1]))
}

pub async fn job_explorer_validate(
    config: &Config,
    opts: &ExplorerValidateOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<ExplorerValidateOutcome> {
    let run_ids = if opts.run_ids.is_empty() {
        None
    } else {
        Some(opts.run_ids.clone())
    };
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;

    let ts = since_ts(opts.since.as_deref());
    let (from_block, to_block) = block_window(&store, ts)?;

    let report = validate::compute_validation(
        &store,
        validate::ValidationQuery {
            chain,
            from_block,
            to_block,
            match_window: opts.match_window,
            run_filter: run_ids.as_deref(),
            threshold_sweep: opts.threshold_sweep,
        },
    )?;

    if opts.json {
        progress.log(&serde_json::to_string_pretty(&report)?);
    } else {
        progress.log(&validate::render_validation_report(&report));
    }

    let mut missing_pools_path = None;
    if opts.emit_missing_pools && !report.missing_pools.is_empty() {
        let path = "results/missing_pools.txt";
        std::fs::create_dir_all("results").ok();
        std::fs::write(path, report.missing_pools.join("\n"))?;
        progress.log(&format!("\nMissing pools written to {path}"));
        missing_pools_path = Some(path.to_string());
    }

    Ok(ExplorerValidateOutcome {
        report,
        missing_pools_path,
    })
}
