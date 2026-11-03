//! ``explorer dashboard`` — forensic static HTML/JSON snapshot from a
//! read-only explorer SQLite (never migrates).
use std::path::{Path, PathBuf};
use std::time::Duration;

use mev_scout_core::config::Config;
use mev_scout_core::explorer::dashboard::{
    build, render_csv, render_html, render_json, BuildOptions, WindowSpec,
};
use mev_scout_core::types::ChainName;

/// CLI flags for one dashboard emission (or a `--repeat` loop).
pub struct DashboardArgs {
    pub db: Option<PathBuf>,
    pub window: String,
    pub out: PathBuf,
    pub format: String,
    pub max_ops: usize,
    pub max_evidence: usize,
    pub with_gaps: bool,
    pub budget_mb: usize,
    pub repeat: Option<Duration>,
}

fn resolve_db(config: &Config, db: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = db {
        return Ok(p.to_path_buf());
    }
    let chain = config
        .chains
        .keys()
        .next()
        .and_then(|k| k.parse::<ChainName>().ok())
        .unwrap_or(ChainName::Avalanche);
    Ok(PathBuf::from(config.effective_explorer_db_path(&chain)))
}

fn emit_once(config: &Config, args: &DashboardArgs) -> anyhow::Result<()> {
    let db_path = resolve_db(config, args.db.as_deref())?;
    let window = WindowSpec::parse(&args.window)?;
    let opts = BuildOptions {
        db_path: db_path.clone(),
        window,
        max_ops: args.max_ops,
        max_evidence: args.max_evidence,
        budget_mb: args.budget_mb,
        with_gaps: args.with_gaps,
        gap_block_limit: 10_000,
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
