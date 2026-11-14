//! ``explorer purge-retired`` — remove historical rows for retired strategies.
use std::path::{Path, PathBuf};

use mev_scout_core::config::Config;
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::types::ChainName;

fn resolve_db(config: &Config, db: Option<&Path>) -> PathBuf {
    if let Some(p) = db {
        return p.to_path_buf();
    }
    let chain = config
        .chains
        .keys()
        .next()
        .and_then(|k| k.parse::<ChainName>().ok())
        .unwrap_or(ChainName::Avalanche);
    PathBuf::from(config.effective_explorer_db_path(&chain))
}

pub async fn cmd_purge_retired(
    config: &Config,
    a: &crate::cli::PurgeRetiredArgs,
) -> anyhow::Result<()> {
    if !a.dry_run && !a.yes {
        anyhow::bail!("refusing to purge without --yes (use --dry-run to preview)");
    }
    let db = resolve_db(config, a.db.as_deref());
    if !db.exists() {
        anyhow::bail!("explorer database not found: {}", db.display());
    }
    eprintln!("Explorer purge-retired — {}", db.display());
    eprintln!("Back up the DB file before running without --dry-run.");

    let store = if a.dry_run {
        ExplorerStore::open_read_only(&db)?
    } else {
        ExplorerStore::open(&db)?
    };
    let report = store.purge_retired_strategies(a.dry_run)?;
    println!(
        "bundler rows: {} ({})",
        report.bundler_rows,
        if a.dry_run { "would delete" } else { "deleted" }
    );
    println!(
        "bad_debt_liq rows: {} ({})",
        report.bad_debt_rows,
        if a.dry_run {
            "would rewrite"
        } else {
            "rewritten"
        }
    );
    if a.dry_run {
        println!("dry run: no changes written");
    }
    Ok(())
}
