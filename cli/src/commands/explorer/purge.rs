//! ``explorer purge-retired`` — remove historical rows for retired strategies.
use mev_scout_core::config::Config;
use mev_scout_core::explorer::store::ExplorerStore;

pub async fn cmd_purge_retired(
    config: &Config,
    a: &crate::cli::PurgeRetiredArgs,
) -> anyhow::Result<()> {
    if !a.dry_run && !a.yes {
        anyhow::bail!("refusing to purge without --yes (use --dry-run to preview)");
    }
    let db = super::resolve_db(config, a.db.as_deref());
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
