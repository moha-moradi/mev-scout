//! Explorer HTML dashboard emission: read-only explorer SQLite → static HTML.
//! Never migrates the DB.
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use mev_scout_core::explorer::dashboard::{build, render_html, BuildOptions, WindowSpec};

const MAX_OPS: usize = 2000;
const MAX_EVIDENCE: usize = 500;
const BUDGET_MB: usize = 8;
const GAP_BLOCK_LIMIT: u64 = 10_000;

pub struct DashboardEmitArgs {
    pub db_path: PathBuf,
    pub out: PathBuf,
    pub window: WindowSpec,
}

pub fn cmd_dashboard(args: &DashboardEmitArgs) -> anyhow::Result<()> {
    let opts = BuildOptions {
        db_path: args.db_path.clone(),
        window: args.window.clone(),
        max_ops: MAX_OPS,
        max_evidence: MAX_EVIDENCE,
        budget_mb: BUDGET_MB,
        with_gaps: false,
        gap_block_limit: GAP_BLOCK_LIMIT,
    };
    let snap = build(&opts)?;
    if let Some(parent) = args.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let body = render_html(&snap)?;
    atomic_write(&args.out, body.as_bytes())?;
    tracing::info!(
        path = %args.out.display(),
        db = %args.db_path.display(),
        window = %args.window.label,
        schema = snap.db.user_version,
        ops = snap.panels.kpi.ops,
        "dashboard written"
    );
    println!("dashboard written: {}", args.out.display());
    open_in_browser(&args.out);
    Ok(())
}

/// Best-effort display; never fails the command. Skipped when stdout is not a
/// terminal so scripted/test invocations do not spawn browser windows.
fn open_in_browser(path: &Path) {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let spawned = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&abs)
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&abs).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&abs).spawn()
    };
    if let Err(e) = spawned {
        tracing::warn!(error = %e, path = %abs.display(), "could not open dashboard");
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("out")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
