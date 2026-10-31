//! `mev-scout explorer` subcommands: index, report, backfill.
use comfy_table::Table;

use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::explorer::store::{ExplorerStore, MevOpRow};
use mev_scout_core::explorer::MevKind;
use mev_scout_core::types::ChainName;
use mev_scout_core::utils::epoch_secs;

// ── shared setup ────────────────────────────────────────────────────────

fn explorer_store(config: &Config, chain: ChainName) -> anyhow::Result<ExplorerStore> {
    ExplorerStore::open(config.effective_explorer_db_path(&chain))
}

fn since_ts(since: Option<&str>) -> u64 {
    let now = epoch_secs();
    match since {
        Some("all") | None => 0,
        Some(other) => match report::parse_duration_secs(other) {
            Some(secs) => now.saturating_sub(secs),
            None => {
                tracing::warn!("unknown --since '{other}', using all");
                0
            }
        },
    }
}

fn short_addr(s: &str) -> String {
    if s.len() == 42 && s.starts_with("0x") {
        format!("{}..{}", &s[..8], &s[s.len() - 6..])
    } else {
        s.to_string()
    }
}

mod backfill;
mod index;
mod report;

pub use backfill::cmd_backfill;
pub use index::cmd_index;
pub use report::cmd_explorer_report;
