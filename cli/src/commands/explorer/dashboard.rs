//! ``explorer --dashboard`` — forensic static HTML/JSON snapshot from an
//! all-time, read-only explorer SQLite (never migrates).
use std::path::{Path, PathBuf};
use std::time::Duration;

use mev_scout_core::config::Config;
use mev_scout_core::explorer::dashboard::{
    build, render_csv, render_html, render_json, BuildOptions, WindowSpec,
};

const MAX_OPS: usize = 2000;
const MAX_EVIDENCE: usize = 500;
const BUDGET_MB: usize = 8;
const GAP_BLOCK_LIMIT: u64 = 10_000;

/// CLI settings for one dashboard emission (or a `--repeat` loop).
pub struct DashboardArgs {
    pub db: Option<PathBuf>,
    pub out: PathBuf,
    pub format: String,
    pub repeat: Option<Duration>,
}

fn emit_once(config: &Config, args: &DashboardArgs) -> anyhow::Result<()> {
    let db_path = super::resolve_db(config, args.db.as_deref());
    let opts = BuildOptions {
        db_path: db_path.clone(),
        window: WindowSpec::all(),
        max_ops: MAX_OPS,
        max_evidence: MAX_EVIDENCE,
        budget_mb: BUDGET_MB,
        with_gaps: false,
        gap_block_limit: GAP_BLOCK_LIMIT,
    };
    let snap = build(&opts)?;
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let body = match args.format.as_str() {
        "json" => render_json(&snap)?,
        "html" => render_html(&snap)?,
        "csv" => render_csv(&snap),
        other => anyhow::bail!("unsupported --format '{other}' (html|json|csv)"),
    };
    atomic_write(&args.out, body.as_bytes())?;
    tracing::info!(
        path = %args.out.display(),
        db = %db_path.display(),
        schema = snap.db.user_version,
        ops = snap.panels.kpi.ops,
        "dashboard written"
    );
    Ok(())
}

/// Write via temp file + rename so `--repeat` readers never see a partial file.
fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("out")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub async fn cmd_dashboard(config: &Config, args: DashboardArgs) -> anyhow::Result<()> {
    emit_once(config, &args)?;
    let Some(period) = args.repeat else {
        return Ok(());
    };
    loop {
        tokio::time::sleep(period).await;
        if let Err(e) = emit_once(config, &args) {
            tracing::error!(error = %e, "dashboard refresh failed");
        }
    }
}
