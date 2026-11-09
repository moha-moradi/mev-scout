//! `mev-scout explorer` subcommands: index (historical), report, dashboard.
use comfy_table::Table;

use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::explorer::store::{ExplorerStore, MevOpRow};
use mev_scout_core::explorer::MevKind;
use mev_scout_core::types::ChainName;

// ── shared setup ────────────────────────────────────────────────────────

fn explorer_store(config: &Config, chain: ChainName) -> anyhow::Result<ExplorerStore> {
    ExplorerStore::open(config.effective_explorer_db_path(&chain))
}

fn short_addr(s: &str) -> String {
    if s.len() == 42 && s.starts_with("0x") {
        format!("{}..{}", &s[..8], &s[s.len() - 6..])
    } else {
        s.to_string()
    }
}

mod backfill;
pub mod dashboard;
mod report;

pub use backfill::cmd_backfill;
pub use dashboard::cmd_dashboard;
pub use report::cmd_explorer_report;

/// Run `explorer dashboard` from clap [`crate::cli::DashboardArgs`].
pub async fn run_dashboard(config: &Config, a: &crate::cli::DashboardArgs) -> anyhow::Result<()> {
    let repeat = match a.repeat.as_deref() {
        None => None,
        Some(s) => Some(
            s.parse::<humantime::Duration>()
                .map_err(|e| anyhow::anyhow!("invalid --repeat '{s}': {e}"))?
                .into(),
        ),
    };
    cmd_dashboard(
        config,
        dashboard::DashboardArgs {
            db: a.db.clone(),
            window: a.window.clone(),
            out: a.out.clone(),
            format: a.format.clone(),
            max_ops: a.max_ops,
            max_evidence: a.evidence,
            with_gaps: a.with_gaps,
            budget_mb: a.budget_mb,
            repeat,
        },
    )
    .await
}
