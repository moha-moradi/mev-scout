//! `mev-scout explorer` subcommands: index, stats, show.

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
mod show;
mod stats;
mod validate;

pub use backfill::cmd_backfill;
pub use index::cmd_index;
pub use report::cmd_explorer_report;
pub use show::cmd_show;
pub use stats::cmd_stats;
pub use validate::cmd_explorer_validate;
