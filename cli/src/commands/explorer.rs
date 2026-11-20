//! `mev-scout explorer` mode: HTML dashboard and index backfill.
use std::path::PathBuf;

use mev_scout_core::config::Config;
use mev_scout_core::explorer::dashboard::WindowSpec;
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::types::ChainName;

mod backfill;
mod dashboard;

pub use backfill::cmd_backfill;
use dashboard::{cmd_dashboard, DashboardEmitArgs};

fn explorer_store(config: &Config, chain: ChainName) -> anyhow::Result<ExplorerStore> {
    ExplorerStore::open(config.effective_explorer_db_path(&chain))
}

/// Explorer DB path for the active chain (config-owned; no CLI override).
pub(crate) fn resolved_db_path(config: &Config) -> PathBuf {
    PathBuf::from(config.effective_explorer_db_path(&config.chain))
}

/// `results/explorer-<chain>-<range>.html`.
fn default_dashboard_out(chain: &str, range_label: &str) -> PathBuf {
    PathBuf::from("results").join(format!(
        "explorer-{}-{}.html",
        file_label(chain),
        file_label(range_label)
    ))
}

/// Restrict to `[a-z0-9._-]` so a label can never introduce path segments.
fn file_label(s: &str) -> String {
    let label: String = s
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if label.trim_matches('-').is_empty() {
        "all".to_string()
    } else {
        label
    }
}

/// Real time span of the indexed blocks, so the dashboard window matches the
/// run's range instead of always being all-time. Falls back to an all-time
/// window (with the range label) when no block timestamps are recorded.
pub(crate) fn index_window(
    config: &Config,
    range_label: &str,
    from_block: u64,
    to_block: u64,
) -> WindowSpec {
    let mut window = WindowSpec::all();
    window.label = range_label.to_string();
    let Ok(store) = ExplorerStore::open_read_only(resolved_db_path(config)) else {
        return window;
    };
    let row = store.connection().query_row(
        "SELECT MIN(ts), MAX(ts) FROM blocks WHERE block_number >= ?1 AND block_number <= ?2",
        [from_block as i64, to_block as i64],
        |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
    );
    if let Ok((Some(min_ts), Some(max_ts))) = row {
        window.since_ts = min_ts;
        window.until_ts = max_ts;
    }
    window
}

/// Label describing an index run's range (also drives the default file name
/// and the dashboard window label).
pub(crate) fn index_range_label(
    window: Option<&str>,
    exact_blocks: bool,
    from_block: u64,
    to_block: u64,
) -> String {
    if exact_blocks {
        format!("blocks-{from_block}-{to_block}")
    } else {
        window.unwrap_or("7d").to_string()
    }
}

/// Emit one HTML dashboard, resolving the output path when `--out` is absent.
pub(crate) async fn emit_dashboard(
    config: &Config,
    out: Option<PathBuf>,
    range_label: &str,
    window: WindowSpec,
) -> anyhow::Result<()> {
    let out = out.unwrap_or_else(|| default_dashboard_out(&config.chain.to_string(), range_label));
    cmd_dashboard(&DashboardEmitArgs {
        db_path: resolved_db_path(config),
        out,
        window,
        chain: config.chain,
        cache_db: PathBuf::from(config.effective_db_path(&config.chain)),
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_db_uses_active_chain() {
        let config = Config {
            chain: ChainName::Avalanche,
            ..Config::default()
        };
        let expected = PathBuf::from(config.effective_explorer_db_path(&ChainName::Avalanche));
        assert_eq!(resolved_db_path(&config), expected);
        assert!(
            !resolved_db_path(&config)
                .to_string_lossy()
                .contains("explorer-bsc"),
            "multi-chain configs must not fall back to the first map key"
        );
    }

    #[test]
    fn index_labels_follow_the_requested_range() {
        assert_eq!(index_range_label(None, false, 0, 0), "7d");
        assert_eq!(index_range_label(Some("24h"), false, 0, 0), "24h");
        assert_eq!(index_range_label(None, true, 10, 20), "blocks-10-20");
    }

    #[test]
    fn auto_output_name_tracks_chain_and_range() {
        assert_eq!(
            default_dashboard_out("avalanche", "all"),
            PathBuf::from("results/explorer-avalanche-all.html")
        );
        assert_eq!(
            default_dashboard_out("Avalanche", " 24H "),
            PathBuf::from("results/explorer-avalanche-24h.html")
        );
        assert_eq!(
            default_dashboard_out("bsc", "blocks-10-20"),
            PathBuf::from("results/explorer-bsc-blocks-10-20.html")
        );
        assert_eq!(
            default_dashboard_out("a/b", "../evil"),
            PathBuf::from("results/explorer-a-b-..-evil.html")
        );
    }
}
